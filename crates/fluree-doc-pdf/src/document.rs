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
/// Most words of a table's title band.
const MAX_TABLE_TITLE_WORDS: usize = 12;

/// Widest, in ems of a band's text, the columns beside a title band may run
/// together and the band still stand across the table. A line turned down a
/// column takes a line's height and its padding, about two ems; a word set
/// across a column takes three or more.
const BAND_MARGIN_EMS: f64 = 2.5;

/// A grid whose first row is the table's title rather than its header: one label alone in the row, as one cell across the
/// whole table (`Hex Cap Screws ASME B18.2.1-2012`, a section band
/// `Mission Critical (10K rpm)`) or as the only text in the first of a row
/// of empty cells (a caption the run took in, `Thermo Scientific™ M Series
/// Centrifuges`), over a row that holds text in two columns or more.
///
/// Taken as the header, the title named every column and the rows that do
/// name the columns read as data. Returns the title's text and the grid
/// from the row under it.
fn table_title(
    g: &table::Grid,
    page: &crate::extract::Page,
    scrub: &[(String, bool)],
    spanning: bool,
) -> Option<(String, table::Grid)> {
    if g.rows() < 3 || g.cols() < 2 {
        return None;
    }
    if let Some(lay) = g.layout.as_deref() {
        return layout_title(g, lay, page, scrub);
    }
    let cols = g.cols();
    let flat = g.cell_texts(&page.glyphs);
    let filled = |r: usize| {
        (0..cols)
            .filter(|&c| !flat[r * cols + c].trim().is_empty())
            .count()
    };
    let mut merges = g.merges(&page.rules, &page.fills);
    g.text_merges(&page.glyphs, &page.rules, &mut merges);
    // One cell across the whole row, or one label in the first column with
    // nothing to its right and nothing in the row spanned: a group label
    // over some of the columns (`Dates` over `Start | Next review`) is the
    // header's, and is spanned over them.
    // A row the alignment read as one cell across (`spanning`) is one too.
    // Or one cell across all of the table but columns too narrow to hold a
    // label set across them ([`BAND_MARGIN_EMS`] of the row's text
    // together): a margin, or the room a label turned down the rows takes,
    // whose rule may run on up through the band (`Average Luminance Table
    // (cd/m²)` over every column but the one `Vertical Angle` is turned
    // in). A group's label over part of the table leaves columns a label
    // fits.
    let em = {
        let mut sizes: Vec<f64> = page
            .glyphs
            .iter()
            .filter(|gl| gl.is_horizontal() && !gl.text.trim().is_empty())
            .filter(|gl| {
                let (x, y) = gl.center();
                g.cell_at(x, y).is_some_and(|(r, _)| r == 0)
            })
            .map(|gl| gl.font_size as f64)
            .collect();
        sizes.sort_by(f64::total_cmp);
        sizes.get(sizes.len() / 2).copied().unwrap_or(0.0)
    };
    let banded = (0..cols)
        .find(|&c| !flat[c].trim().is_empty())
        .is_some_and(|c0| {
            let mut end = c0 + 1;
            while end < cols && merges.continues_left[end] {
                end += 1;
            }
            let held = (0..cols)
                .filter(|&c| !flat[c].trim().is_empty())
                .all(|c| c >= c0 && c < end);
            let outside = (g.xs[c0] - g.xs[0]) + (g.xs[cols] - g.xs[end]);
            held && end - c0 >= 2 && outside > 0.0 && outside < BAND_MARGIN_EMS * em
        });
    let across = spanning || (1..cols).all(|c| merges.continues_left[c]) || banded;
    let alone = filled(0) == 1
        && !flat[0].trim().is_empty()
        && (1..cols).all(|c| !merges.continues_left[c]);
    if !(across || alone) || filled(1) < 2 {
        return None;
    }
    // Across the row, a title is one label, or set as running text: a line
    // of it, its words at their ordinary spacing, runs over the cuts
    // between columns (`Hex Cap Screws` over four of them). Labels set
    // apart, each within its column, are the columns' own (`Area |
    // Competence` under one ruled cell).
    let row_lines = || {
        let inside: Vec<crate::glyph::Glyph> = page
            .glyphs
            .iter()
            .filter(|gl| {
                gl.bbox.is_some() && !gl.text.trim().is_empty() && gl.is_horizontal() && {
                    let (x, y) = gl.center();
                    g.cell_at(x, y).is_some_and(|(r, _)| r == 0)
                }
            })
            .cloned()
            .collect();
        crate::line::assemble(&inside)
    };
    let lines = if across { row_lines() } else { Vec::new() };
    let runs_across = lines.iter().any(|l| {
        g.xs[1..g.xs.len() - 1]
            .iter()
            .any(|&x| l.bbox.x0 < x - 1.0 && l.bbox.x1 > x + 1.0)
    });
    let text = if across && runs_across {
        crate::block::build(lines).text()
    } else if spanning {
        g.row_text(&page.glyphs, 0)
    } else if (across || alone) && filled(0) == 1 {
        (0..cols)
            .map(|c| flat[c].trim())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        return None;
    };
    let text = furniture::scrub_cell(
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
        scrub,
    );
    let words = text.split_whitespace().count();
    if words == 0
        || words > MAX_TABLE_TITLE_WORDS
        || table::is_numeric_cell(&text)
        || !text.chars().any(char::is_alphabetic)
    {
        return None;
    }
    let mut trimmed = g.clone();
    trimmed.ys.remove(0);
    trimmed.bbox.y0 = trimmed.ys[0];
    Some((text, trimmed))
}

/// [`table_title`] for a table read from its text ([`crate::textgrid`]):
/// its first row is one label read whole across the row, set as a title
/// (opening on a capital or a figure, with no colon, at the table's left
/// edge or centred on it), over a row with text in two columns or more,
/// and the only row the reading took for the header. A catalogue's
/// band (`ENGINE BEARINGS` over `Rod Bearing | 2001-2002 | …`) opens a
/// table the page cut at it, and the reading took the band for the header.
///
/// The table goes on from the row under it, its layout moved up a row: its
/// spans, the labels set down its rows and its row bands with it. The
/// header the reading found under the band stays the header.
fn layout_title(
    g: &table::Grid,
    lay: &table::TableLayout,
    page: &crate::extract::Page,
    scrub: &[(String, bool)],
) -> Option<(String, table::Grid)> {
    let (rows, cols) = (g.rows(), g.cols());
    if lay.cells.len() != rows * cols {
        return None;
    }
    let texts = lay.texts(&page.glyphs, cols);
    let filled = |r: usize| {
        (0..cols)
            .filter(|&c| !texts[r * cols + c].trim().is_empty())
            .count()
    };
    // One cell across the whole row. A first cell alone in its row is, in a
    // table read from its text, as often a line wrapped from the header
    // above (`share before cumulative`) or a value cut off its row.
    let across = lay
        .spans
        .iter()
        .any(|&(r, c0, c1)| r == 0 && c0 == 0 && c1 >= cols);
    if !across || filled(0) != 1 || texts[0].trim().is_empty() || filled(1) < 2 {
        return None;
    }
    // A banner over the column names the reading found under it heads the
    // header (`December 31, 2017` over `2017 | 2016`), not the table. Nor
    // does the first block's title over its own column names, as each later
    // block's does (`Uganda 1995 2011` over `<16 | 16/17`).
    if lay.header_rows > 1 || lay.restated_headers.contains(&1) {
        return None;
    }
    // Set where a title is: at the table's left edge, or centred on it. A
    // line cut off a row's cell and read across the row sits where that
    // cell's text does.
    let ink: Vec<crate::geom::BBox> = lay.cells[..cols]
        .iter()
        .flatten()
        .filter_map(|&i| page.glyphs.get(i).and_then(|gl| gl.bbox))
        .collect();
    let (x0, x1) = ink.iter().fold((f64::MAX, f64::MIN), |(a, b), bb| {
        (a.min(bb.x0), b.max(bb.x1))
    });
    let size = lay.cells[0]
        .first()
        .and_then(|&i| page.glyphs.get(i))
        .map_or(10.0, |gl| gl.font_size.max(1.0) as f64);
    let width = g.bbox.x1 - g.bbox.x0;
    let at_edge = (x0 - g.bbox.x0).abs() <= size * 1.5;
    let centred = ((x0 + x1) / 2.0 - (g.bbox.x0 + g.bbox.x1) / 2.0).abs() <= width * 0.1;
    if ink.is_empty() || !(at_edge || centred) {
        return None;
    }
    let text = furniture::scrub_cell(
        texts[0]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
        scrub,
    );
    let words = text.split_whitespace().count();
    // Set as a title is: opening on a capital or a figure, and not a field
    // or a line run on (`Grand Total:$459.72`, `from operating activities:`).
    let titled = text.starts_with(|c: char| c.is_uppercase() || c.is_ascii_digit())
        && !text.ends_with([':', ','])
        && !text.contains(':');
    if words == 0
        || words > MAX_TABLE_TITLE_WORDS
        || !titled
        || table::is_numeric_cell(&text)
        || !text.chars().any(char::is_alphabetic)
    {
        return None;
    }
    let up = |r: usize| r.checked_sub(1);
    let mut trimmed = g.clone();
    trimmed.ys.remove(0);
    trimmed.bbox.y0 = trimmed.ys[0];
    trimmed.layout = Some(std::sync::Arc::new(table::TableLayout {
        header_rows: lay.header_rows.saturating_sub(1),
        spans: lay
            .spans
            .iter()
            .filter_map(|&(r, c0, c1)| up(r).map(|r| (r, c0, c1)))
            .collect(),
        downs: lay
            .downs
            .iter()
            .filter_map(|&(r0, c, r1)| {
                let (r0, r1) = (r0.saturating_sub(1), up(r1)?);
                (r1 > r0 + 1).then_some((r0, c, r1))
            })
            .collect(),
        sub_headers: lay.sub_headers.iter().filter_map(|&r| up(r)).collect(),
        restated_headers: lay.restated_headers.iter().filter_map(|&r| up(r)).collect(),
        cells: lay.cells[cols..].to_vec(),
        claimed: lay.claimed.clone(),
    }));
    Some((text, trimmed))
}

/// Rows opening a table nested in a table's rows: the first cell's label
/// set down the rows under it (`Gradient` in a list of conditions), the
/// row's other cells naming columns (`Time (min) | A% | B%`) that the rows
/// in the label's span fill with figures. Set plain or bold, the row is
/// the nested table's header.
///
/// A group's first row of data holds a figure among its values, or names
/// nothing the rows under it fill with figures.
fn nested_header_rows(
    rows: &[Vec<String>],
    continues_above: &[bool],
    header_rows: usize,
) -> Vec<usize> {
    let cols = rows.first().map_or(0, Vec::len);
    if cols < 3 || continues_above.len() != rows.len() * cols {
        return Vec::new();
    }
    let mut out = Vec::new();
    for r in header_rows..rows.len() {
        let label = rows[r][0].trim();
        if label.is_empty() || table::is_numeric_cell(label) {
            continue;
        }
        // The label's span: the rows its cell runs on through, two at least.
        let mut end = r;
        while end + 1 < rows.len() && continues_above[(end + 1) * cols] {
            end += 1;
        }
        if end < r + 2 {
            continue;
        }
        let named: Vec<usize> = (1..cols)
            .filter(|&c| !rows[r][c].trim().is_empty())
            .collect();
        let names: Vec<&str> = named.iter().map(|&c| rows[r][c].trim()).collect();
        let distinct: std::collections::HashSet<&&str> = names.iter().collect();
        // Words, more letters than digits: a count is a value (`N=175` over
        // a subgroup's rates), a name is not (`A%`, `IL-8 (pg/mL)`).
        let worded = |t: &str| {
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            letters > t.chars().filter(|c| c.is_ascii_digit()).count()
        };
        if names.len() < 2
            || distinct.len() != names.len()
            || names
                .iter()
                .any(|t| !worded(t) || table::is_numeric_cell(t))
        {
            continue;
        }
        // Figures under every name, on most of the span's rows.
        let figures = (r + 1..=end)
            .filter(|&k| {
                named
                    .iter()
                    .all(|&c| table::is_numeric_cell(rows[k][c].trim()))
            })
            .count();
        if figures * 2 > end - r {
            out.push(r);
        }
    }
    out
}

fn names_spanned_columns(
    rows: &[Vec<String>],
    continues_left: &[bool],
    continues_above: &[bool],
) -> bool {
    let cols = rows.first().map(Vec::len).unwrap_or(0);
    if rows.len() < 3 || continues_left.len() < 2 * cols {
        return false;
    }
    let empty = |r: usize, c: usize| rows[r].get(c).is_none_or(|t| t.trim().is_empty());
    // A span the ruling leaves open over a column its label does not
    // reach, empty in both rows (the stub's, its rule not drawn up through
    // the band), names the columns from where the label stands. A span
    // with no label over it, cells the ruling leaves open over the stub,
    // names nothing.
    let spans: Vec<(usize, usize)> = header_spans(&continues_left[..cols])
        .into_iter()
        .map(|(a, b)| {
            let a = (a..b).find(|&c| !(empty(0, c) && empty(1, c))).unwrap_or(b);
            (a, b)
        })
        .filter(|&(a, b)| b > a && (a..b).any(|c| !empty(0, c)))
        .collect();
    let spanned = |c: usize| spans.iter().any(|&(a, b)| (a..b).contains(&c));
    // Keys in figures (`0° | 45° | 90°`, sizes, years) name the columns
    // under a span as surely as words, where the row's own stub is empty
    // and the rows under it have one: a row of the data names its row in
    // the stub, and so does the row of figures under a banner.
    let stubs: Vec<usize> = (0..cols).filter(|&c| !spanned(c)).collect();
    let keyed = !stubs.is_empty()
        && stubs.iter().all(|&c| empty(1, c))
        && rows[2..]
            .iter()
            .enumerate()
            .filter(|(i, _)| stubs.iter().any(|&c| !empty(i + 2, c)))
            .count()
            * 2
            >= rows.len() - 2;
    let labels = |a: usize, b: usize| {
        let names: Vec<&str> = rows[1][a..b].iter().map(|t| t.trim()).collect();
        let short = b - a < cols || names.iter().all(|t| t.split_whitespace().count() <= 4);
        short
            && (a + 1..b).all(|k| !continues_left[cols + k])
            && names
                .iter()
                .all(|t| !t.is_empty() && (keyed || !table::is_numeric_cell(t)))
            && names.iter().collect::<std::collections::HashSet<_>>().len() == names.len()
    };
    // A column the first row names without spanning it may take a second
    // label under the first (`This study` over `MAF (411 patients)`), but
    // a part number of its own there (`FPWINGRF-JP2` under `Product No.`,
    // beside two values under a `Type` set across two columns) makes the row
    // the first of the data. Where the ruling runs the first row's label
    // down through the second, the second holds nothing of its own.
    let code = |t: &str| {
        !t.contains(char::is_whitespace)
            && t.chars().any(|c| c.is_ascii_digit())
            && t.chars().any(char::is_alphabetic)
    };
    let named_above = (0..cols).filter(|&c| !spanned(c)).all(|c| {
        let t = rows[1][c].trim();
        t.is_empty()
            || rows[0][c].trim().is_empty()
            || continues_above.get(cols + c).copied().unwrap_or(false)
            || !code(t)
    });
    !spans.is_empty()
        && named_above
        && spans.iter().all(|&(a, b)| labels(a, b))
        && rows[1]
            .iter()
            .all(|t| t.trim().is_empty() || keyed || !table::is_numeric_cell(t.trim()))
}

/// Words a row of a one-column grid holds to read as a sentence, not an
/// entry: a list's entries are a name and a few words of what it is.
const SENTENCE_WORDS: usize = 12;

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
/// two-column list, where a field left empty looks the same. A label over
/// another group label is a group too, over its sub-group.
fn group_label(rows: &[Vec<String>], r: usize) -> bool {
    let (Some(row), Some(next)) = (rows.get(r), rows.get(r + 1)) else {
        return false;
    };
    let label = row[0].trim();
    let below = next[0].trim();
    // Over a full row, or over a group of its own: a group over its
    // sub-group (`Power Management` over `Average power requirements (W)`
    // over `Operational | 6.7W | …`).
    row.len() >= 3
        && !label.is_empty()
        && !table::is_numeric_cell(label)
        && row[1..].iter().all(|t| t.trim().is_empty())
        && (next.iter().all(|t| !t.trim().is_empty()) || group_label(rows, r + 1))
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

/// What a heading element's id opens with until elements are numbered,
/// then its page and block: `\0h3:12`. A NUL opens no id an element
/// is given.
const HEADING_BLOCK: &str = "\0h";

/// Most characters a line taken for the document's title in place of the
/// one first read may hold.
const MAX_TITLE_CHARS: usize = 200;

/// A field's line: a short label, its colon, and a code after it, figures
/// and no word (`Amazon.com order number: 114-0619372-9653019`). Its code
/// can name a file as its title does, and it is no title. A title with a
/// colon of its own goes on in words (`Dehydrobrachylaenolide: an
/// eudesmane-type sesquiterpene lactone`).
fn field_line(text: &str) -> bool {
    text.split_once(':').is_some_and(|(label, value)| {
        let coded = value.chars().any(|c| c.is_ascii_digit())
            && !value
                .split(|c: char| !c.is_alphabetic())
                .any(|w| w.chars().count() > MAX_CODE_LETTERS);
        coded && label.split_whitespace().count() <= MAX_FIELD_LABEL_WORDS
    })
}

/// Most letters a run of a code holds (`KP`, `USD`): a longer run is a word.
const MAX_CODE_LETTERS: usize = 3;

/// Most words a field's label runs to.
const MAX_FIELD_LABEL_WORDS: usize = 4;

/// The document's title, emitted as a `doco:Title` only where it is sure:
/// a wrong title becomes what consumers index the document by, and no
/// title is better than a wrong one. DoCO holds a title as the document's
/// header, apart from the sections, as a Word document's title and a deck's
/// are held. The title is read as a heading until here, for the passes
/// above that weigh headings by it.
///
/// Sure, by its evidence:
/// - `metadata`: the file's own title (`/Title`) names it.
/// - `outline`: a bookmark at the outline's first depth names it.
/// - `type`: set apart as the page's title by its type, and nothing doubts
///   it ([`heading::title_doubt`]).
///
/// Where the file's title names another heading on the first page and not
/// the line taken, that heading is the title: a journal's mark (`materials`)
/// set larger than the article's title, a pharmacy's name over a drug's
/// label. A line in doubt stays a level-1 heading.
///
/// Around a title that is sure, its subtitle ([`read_subtitle`]) and its
/// kickers ([`settle_kickers`]) are read. `block` gives a heading's block,
/// and `body` the body's size.
fn settle_title<'a>(
    elements: &mut [Element],
    meta: Option<&str>,
    outline: &[OutlineItem],
    block: impl Fn(&Element) -> Option<&'a Block>,
    body: f32,
) {
    let meta = meta.filter(|m| heading::meaningful_title(m));
    let bookmarked = |text: &str| {
        let top = outline.iter().map(|o| o.level).min().unwrap_or(1);
        outline
            .iter()
            .filter(|o| o.level == top)
            .any(|o| heading::names_title(&o.title, text) && heading::names_title(text, &o.title))
    };
    let debug = std::env::var_os("FDOC_TITLE_DEBUG").is_some();
    for i in 0..elements.len() {
        if elements[i].kind != "doco:SectionTitle" || elements[i].evidence != "title" {
            continue;
        }
        let text = elements[i].text.clone();
        let doubt = heading::title_doubt(&text);
        // A date is no title, whatever names it: a meeting's transcript
        // states its days in its title too.
        let dated = doubt == Some("date");
        let mut decision: (&str, &'static str) = ("keep", "type");
        if !dated && meta.is_some_and(|m| heading::names_title(m, &text)) {
            decision = ("keep", "metadata");
        } else if !dated && bookmarked(&text) {
            decision = ("keep", "outline");
        } else if !dated && meta.is_some_and(|m| heading::part_of_title(m, &text)) {
            decision = ("keep", "metadata");
        } else {
            // A line of the first page the file's title names instead: a
            // heading, or a short paragraph set as the title is.
            let named = meta.and_then(|m| {
                (0..elements.len()).find(|&k| {
                    k != i
                        && elements[k].page == elements[i].page
                        && matches!(
                            elements[k].kind.as_str(),
                            "doco:SectionTitle" | "doco:Paragraph"
                        )
                        && elements[k].text.chars().count() <= MAX_TITLE_CHARS
                        && !field_line(&elements[k].text)
                        && heading::names_title(m, &elements[k].text)
                })
            });
            match named {
                Some(k) => {
                    decision = ("swap", "metadata");
                    let e = &mut elements[k];
                    e.kind = "doco:Title".into();
                    e.level = None;
                    e.evidence = "metadata";
                }
                None => {
                    if let Some(why) = doubt {
                        decision = ("doubt", why);
                    }
                }
            }
        }
        if debug {
            eprintln!(
                "TITLE\t{}\t{}\t{:.2}\t{}\t{}",
                decision.0,
                decision.1,
                block(&elements[i]).map_or(0.0, |b| b.font_size / body.max(1.0)),
                text,
                meta.unwrap_or("")
            );
        }
        let e = &mut elements[i];
        if decision.0 == "keep" {
            e.kind = "doco:Title".into();
            e.level = None;
            e.evidence = decision.1;
        } else {
            e.evidence = "font-size";
            e.level = Some(1);
        }
    }
    read_subtitle(elements, &block);
    settle_kickers(elements);
}

/// Most lines a subtitle runs to.
const MAX_SUBTITLE_LINES: usize = 2;

/// Largest a subtitle is set, against its title: a step down. A line as
/// large as the title is more of it.
const SUBTITLE_STEP: f32 = 0.95;

/// Most space between a title and its subtitle, in the title's size: a
/// subtitle is set close under its title, as one more line of the display.
/// A heading set further down opens the text.
const SUBTITLE_GAP: f64 = 1.0;

/// The title's subtitle: an explanatory or alternative title set directly
/// under it (`July 2026` under `Monetary Policy Report`, `Quarries
/// Regulations 1999` under a guide's title, `LED Wall Pack` under a product
/// series), a `doco:Subtitle`. Read until here as a heading by its size, it
/// opened a section holding the front matter or nothing.
///
/// The element after the document's title is its subtitle when:
/// - it is a heading by its size alone, on the title's page;
/// - it is set close under the title and across it, a step smaller than the
///   title, in one or two lines;
/// - nothing marks it a section's head (a number, a section's or a
///   division's name, a lead-in) or the authors' names.
///
/// How the rest of the document's headings are set says nothing of it: a
/// deck sets its title slide's date as its slides' headings, and a spec
/// sheet its product line as its sections' heads.
fn read_subtitle<'a>(elements: &mut [Element], block: &impl Fn(&Element) -> Option<&'a Block>) {
    let Some(t) = elements.iter().position(|e| e.kind == "doco:Title") else {
        return;
    };
    let s = t + 1;
    let (Some(title), Some(sub)) = (block(&elements[t]), elements.get(s).and_then(|e| block(e)))
    else {
        return;
    };
    let e = &elements[s];
    if e.kind != "doco:SectionTitle" || e.evidence != "font-size" || e.page != elements[t].page {
        return;
    }
    let size = title.font_size as f64;
    let under = sub.bbox.y0 >= title.bbox.y1 - size * 0.2
        && sub.bbox.y0 - title.bbox.y1 <= size * SUBTITLE_GAP
        && sub.bbox.x1.min(title.bbox.x1) > sub.bbox.x0.max(title.bbox.x0);
    let set =
        sub.font_size <= title.font_size * SUBTITLE_STEP && sub.lines.len() <= MAX_SUBTITLE_LINES;
    let sectional = matches!(
        heading::title_doubt(&e.text),
        Some("numbered" | "section-name" | "division" | "continued" | "lead-in")
    ) || heading::authors_line(&e.text);
    if std::env::var_os("FDOC_TITLE_DEBUG").is_some() {
        eprintln!(
            "SUBTITLE\t{}\t{}\t{}\t{}\t{}\t{}",
            under && set && !sectional,
            under,
            set,
            sectional,
            e.text,
            elements[t].text
        );
    }
    if under && set && !sectional {
        let e = &mut elements[s];
        e.kind = "doco:Subtitle".into();
        e.level = None;
        e.evidence = "subtitle";
    }
}

/// A kicker is a kicker over the document's title. Over a line in doubt,
/// which stays a heading, it is text as any line is.
fn settle_kickers(elements: &mut [Element]) {
    for i in 0..elements.len() {
        if elements[i].evidence != "kicker" {
            continue;
        }
        let over_title = elements[i + 1..]
            .iter()
            .find(|e| e.evidence != "kicker")
            .is_some_and(|e| e.kind == "doco:Title");
        if !over_title {
            elements[i].evidence = "layout";
        }
    }
}

/// How far apart, in points, two parts' column edges may be drawn and still
/// be one table's columns. A table the page break splits is drawn by one
/// layout on both pages, so its edges agree to within a rule's stroke.
const CONTINUED_EDGE_TOLERANCE: f64 = 3.0;

/// Number `elements` in emission order, `elem-00001` on, after a pass added
/// or removed some, and keep each continued table naming the part before it.
/// A part removed with its element leaves the one after it continuing
/// nothing, rather than naming whatever element took its id.
pub(crate) fn renumber(elements: &mut [Element]) {
    let mut renamed = std::collections::HashMap::new();
    for (i, e) in elements.iter_mut().enumerate() {
        let id = format!("elem-{:05}", i + 1);
        let old = std::mem::replace(&mut e.id, id.clone());
        renamed.insert(old, id);
    }
    for e in elements.iter_mut() {
        e.continues = e.continues.take().and_then(|c| renamed.get(&c).cloned());
    }
}

/// An element the analysis minted as an address for a reader's text (a
/// routed region, a table to confirm, a table the layout found and the
/// rules did not), not content of the page.
fn is_splice_anchor(e: &Element) -> bool {
    matches!(e.evidence, "route" | "table-confidence" | "table-missing")
}

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
///
/// A splice anchor between the parts is not something between them: it is
/// an address the reader's text lands at, and the splice drops it. A
/// table's anchor goes in just before the table, so with anchors on the
/// second part follows its own anchor, not the first part, and comparing
/// neighbours alone lost the link on every run that minted anchors.
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
    let mut last = None;
    for i in 0..elements.len() {
        if is_splice_anchor(&elements[i]) {
            continue;
        }
        let Some(p) = last.replace(i) else {
            continue;
        };
        let (before, after) = elements.split_at_mut(i);
        if before[p].kind == "doco:Table"
            && after[0].kind == "doco:Table"
            && after[0].page == before[p].page
        {
            if let (Some(a), Some(b)) = (frame(&before[p]), frame(&after[0])) {
                link_header_box(&mut before[p], &mut after[0], a, b);
                link_rows_below(&before[p], &mut after[0], a, b);
            }
            continue;
        }
        let (prev, next) = (&before[p], &mut after[0]);
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

/// Which cells start a run of merged cells whose value is set across it:
/// row-major, the run's first cell `true`.
///
/// A value centred across two models' columns (`2 years` under `M17
/// Series` and `M21 Series`) is each column's; a run the ruling merely
/// leaves open (a statement's `$ 11,452` under its first column, the next
/// columns unruled on that row) holds its value in its first column. So a
/// run is spread when its only text, in its first cell, is centred on the
/// run, and narrower than most of it: a row of figures the grid failed to
/// part runs the run's whole width, and is no one value. A wider value is
/// spread where it stands in from both edges alike, by an em or more. Only
/// a value with a word in it: figures alone across columns are the grid's
/// cuts off the page's.
fn spread_runs(
    g: &table::Grid,
    glyphs: &[Glyph],
    rows: &[Vec<String>],
    left: &[bool],
) -> Vec<bool> {
    let cols = g.cols();
    let n = rows.len();
    let mut out = vec![false; n * cols];
    if cols < 2 || left.len() != n * cols || g.xs.len() != cols + 1 {
        return out;
    }
    let mut homes: Option<Vec<Option<usize>>> = None;
    for (r, row) in rows.iter().enumerate() {
        let mut c = 1;
        while c < cols {
            let mut end = c + 1;
            while end < cols && left[r * cols + end] {
                end += 1;
            }
            let value = row[c].trim();
            let only_first =
                !value.is_empty() && row[c + 1..end].iter().all(|t| t.trim().is_empty());
            // A value with no word in it, figures and signs alone, set across
            // columns stands in one of them, the grid's cuts off the page's:
            // no datasheet sets one `5.5` for its MIN and its NOM, and a row
            // of figures the grid failed to part is the columns' values.
            let worded = value.chars().any(char::is_alphabetic);
            if end > c + 1 && only_first && worded {
                let cells: Vec<usize> = (c..end).map(|k| r * cols + k).collect();
                let ids: Vec<usize> = match g.layout.as_deref() {
                    Some(lay) => cells
                        .iter()
                        .filter_map(|&i| lay.cells.get(i))
                        .flatten()
                        .copied()
                        .collect(),
                    None => {
                        let homes = homes.get_or_insert_with(|| g.glyph_cells(glyphs));
                        homes
                            .iter()
                            .enumerate()
                            .filter(|(_, h)| h.is_some_and(|i| cells.contains(&i)))
                            .map(|(j, _)| j)
                            .collect()
                    }
                };
                let marks: Vec<&Glyph> = ids
                    .iter()
                    .filter_map(|&j| glyphs.get(j))
                    .filter(|gl| !gl.text.trim().is_empty() && gl.bbox.is_some())
                    .collect();
                let ink = marks
                    .iter()
                    .filter_map(|gl| gl.bbox)
                    .reduce(|a, b| a.union(&b));
                if let Some(ink) = ink {
                    let (x0, x1) = (g.xs[c], g.xs[end]);
                    let width = (x1 - x0).max(1.0);
                    let mid = (ink.x0 + ink.x1) / 2.0;
                    let centred = (mid - (x0 + x1) / 2.0).abs() <= width * SPREAD_CENTRE;
                    let narrow = ink.x1 - ink.x0 <= width * SPREAD_WIDTH;
                    // Wider, it is centred as surely where it stands in from
                    // both edges alike, an em or more: a weight's lines
                    // across two models' columns. A value set from the left
                    // that fills the run stands in by the cell's padding
                    // alone, under an em.
                    let mut sizes: Vec<f64> = marks.iter().map(|gl| gl.font_size as f64).collect();
                    sizes.sort_by(f64::total_cmp);
                    let em = sizes.get(sizes.len() / 2).copied().unwrap_or(0.0);
                    let (l, rgt) = (ink.x0 - x0, x1 - ink.x1);
                    let even = (l - rgt).abs() <= SPREAD_EVEN * em && l.min(rgt) >= em;
                    out[r * cols + c] = centred && (narrow || even);
                }
            }
            c = end;
        }
    }
    out
}

/// How far a spread value's middle may stand from its run's, as a share of
/// the run's width.
const SPREAD_CENTRE: f64 = 0.15;

/// Widest a spread value's ink runs, as a share of its run's width.
const SPREAD_WIDTH: f64 = 0.8;

/// Most a wider spread value's margins in its run may differ, in ems of its
/// text. A typesetter centring a value leaves the same space either side of
/// its advance; the ink measured from the glyph boxes differs from that by
/// the side bearings of its first and last glyphs, a fraction of an em.
const SPREAD_EVEN: f64 = 0.25;

/// How many rows head a grid of figures as its parameters do: each a
/// name ending in a colon and only figures beside it, over three rows or
/// more of figures that are not (`RC %: | 80`, `RW %: | 70 | 50 | 30 | 10`
/// over `RCR: 0 | 119 | 119 | …`, a sweep's `Speed (rpm): | 1500 | 2500 |
/// 3500` over the readings at each speed). The parameters' values stand
/// over the columns the body's figures fill, and the row nearest the body
/// names two of them or more, half or more: it sets the value each column
/// of figures is for. A list of fields over a grid gives each field one
/// value, and a table that is such rows all the way down is a list of
/// fields; none of their rows heads it.
fn parameter_rows(rows: &[Vec<String>]) -> usize {
    let parameter = |row: &Vec<String>| {
        let name = row.first().map_or("", |t| t.trim());
        let values: Vec<&str> = row[1..]
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        name.ends_with(':')
            && name.chars().any(char::is_alphabetic)
            && !values.is_empty()
            && values.iter().all(|t| table::is_numeric_cell(t))
    };
    let k = rows.iter().take_while(|row| parameter(row)).count();
    let body = &rows[k..];
    let figures = |row: &&Vec<String>| {
        let values: Vec<&str> = row[1..]
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        !values.is_empty() && values.iter().all(|t| table::is_numeric_cell(t))
    };
    if k == 0 || body.len() < 3 || body.iter().filter(figures).count() * 2 < body.len() {
        return 0;
    }
    let held = |row: &Vec<String>| -> Vec<usize> {
        (1..row.len())
            .filter(|&c| !row[c].trim().is_empty())
            .collect()
    };
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let filled: Vec<usize> = (1..width)
        .filter(|&c| {
            body.iter()
                .filter(|r| r.get(c).is_some_and(|t| !t.trim().is_empty()))
                .count()
                * 2
                >= body.len()
        })
        .collect();
    let over_body = rows[..k]
        .iter()
        .all(|r| held(r).iter().all(|c| filled.contains(c)));
    let nearest = held(&rows[k - 1]);
    if !over_body || nearest.len() < 2 || nearest.len() * 2 < filled.len() {
        return 0;
    }
    k
}

/// Drop the columns no cell starts in, from every part of a table.
///
/// A column whose cell on every row is empty and the rest of the one to its
/// left is none of the table's: the grid cut it where another row's ruling,
/// a band over the header or a nested table, parts the columns. An
/// environmental data sheet's three-column VOC tables were ruled as six and
/// written with an empty column after each, their cells in columns 0, 2 and
/// 4. A table the page break split is one table: a column goes only where
/// it goes from every part, so the parts keep one set of columns and read
/// on as one.
fn drop_merged_columns(elements: &mut [Element]) {
    let index: std::collections::HashMap<String, usize> = elements
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.clone(), i))
        .collect();
    // Each part's first part: the table it carries on.
    let first = |mut i: usize| {
        let mut seen = 0;
        while let Some(&p) = elements[i].continues.as_ref().and_then(|c| index.get(c)) {
            i = p;
            seen += 1;
            if seen > elements.len() {
                break;
            }
        }
        i
    };
    let mut chains: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for (i, e) in elements.iter().enumerate() {
        if e.kind == "doco:Table" && e.cells.is_some() {
            chains.entry(first(i)).or_default().push(i);
        }
    }
    for parts in chains.values() {
        let width = |i: usize| {
            elements[i]
                .cells
                .as_ref()
                .and_then(|r| r.first())
                .map_or(0, Vec::len)
        };
        let cols = width(parts[0]);
        if cols < 2 || parts.iter().any(|&i| width(i) != cols) {
            continue;
        }
        // A column stays where any part has a cell that starts in it.
        let starts = |i: usize, c: usize| {
            let e = &elements[i];
            let rows = e.cells.as_ref().unwrap();
            let left = e.merged_left.as_deref();
            rows.iter().enumerate().any(|(r, row)| {
                !row[c].trim().is_empty()
                    || !left.is_some_and(|m| m.get(r * cols + c).copied().unwrap_or(false))
            })
        };
        let keep: Vec<bool> = (0..cols)
            .map(|c| c == 0 || parts.iter().any(|&i| starts(i, c)))
            .collect();
        if keep.iter().all(|k| *k) {
            continue;
        }
        for &i in parts {
            let e = &mut elements[i];
            let squeeze = |flags: &Vec<bool>| -> Vec<bool> {
                flags
                    .chunks(cols)
                    .flat_map(|row| row.iter().zip(&keep).filter(|(_, k)| **k).map(|(f, _)| *f))
                    .collect()
            };
            if let Some(rows) = e.cells.as_mut() {
                for row in rows.iter_mut() {
                    let mut c = 0;
                    row.retain(|_| {
                        c += 1;
                        keep[c - 1]
                    });
                }
            }
            e.merged_left = e.merged_left.as_ref().map(squeeze);
            e.merged_down = e.merged_down.as_ref().map(squeeze);
            e.spread_left = e.spread_left.as_ref().map(squeeze);
            if let Some(rows) = e.cells.as_ref() {
                e.text = rows
                    .iter()
                    .map(|r| r.join(" | "))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
    }
}

/// Widest gap, in points, between a header drawn as a box of its own and
/// the box of rows under it: the space a layout leaves between two boxes,
/// not a paragraph's.
const HEADER_BOX_GAP: f64 = 24.0;

/// Link a header drawn as a box of its own to the rows boxed under it.
///
/// A spreadsheet laid out for print can fill its header in one box and
/// rule its rows in another a gap below, each read as a table: the header
/// one names nothing, and the rows have no names. The upper part is the
/// lower one's header where the two follow one another with nothing
/// between, share their outer edges, every upper column edge is a lower
/// one, the upper part sets names and no figures, and the lower sets
/// figures. A name over two of the lower part's columns heads both
/// (`Nominal Size` over the size and its decimal), so the upper part is
/// dealt onto the lower one's columns, a name spanning the columns it
/// covers, and it is all header.
fn link_header_box(prev: &mut Element, next: &mut Element, a: &TableFrame, b: &TableFrame) {
    let tol = CONTINUED_EDGE_TOLERANCE;
    let (Some(pa), Some(pb)) = (prev.bbox, next.bbox) else {
        return;
    };
    let (Some(&a0), Some(&a1), Some(&b0), Some(&b1)) =
        (a.xs.first(), a.xs.last(), b.xs.first(), b.xs.last())
    else {
        return;
    };
    // An upper edge is the lower edge nearest it, within a quarter of the
    // narrowest lower column: no other edge of the lower part is meant. A
    // box drawn a row at a time has its sides in pieces a little apart.
    let narrowest =
        b.xs.windows(2)
            .map(|w| w[1] - w[0])
            .fold(f64::MAX, f64::min);
    let reach = (narrowest / 4.0).max(tol);
    let gap = pb.y0 - pa.y1;
    if !(-tol..=HEADER_BOX_GAP).contains(&gap)
        || (a0 - b0).abs() > reach
        || (a1 - b1).abs() > reach
        || !a
            .xs
            .iter()
            .all(|x| b.xs.iter().any(|y| (x - y).abs() <= reach))
        || next.continues.is_some()
    {
        return;
    }
    let (Some(head), Some(rows)) = (prev.cells.as_ref(), next.cells.as_ref()) else {
        return;
    };
    let names = head
        .iter()
        .flatten()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .all(|t| t.chars().any(char::is_alphabetic) && !table::is_numeric_cell(t));
    let figures = rows
        .first()
        .is_some_and(|r| r.iter().any(|t| table::is_numeric_cell(t.trim())));
    if !names || !figures || head.is_empty() {
        return;
    }
    // Each lower column under the upper column its middle falls in.
    let cols = b.xs.len() - 1;
    let under: Vec<usize> = (0..cols)
        .map(|j| {
            let mid = (b.xs[j] + b.xs[j + 1]) / 2.0;
            a.xs.windows(2)
                .position(|w| mid >= w[0] && mid < w[1])
                .unwrap_or(0)
        })
        .collect();
    let old_cols = a.xs.len() - 1;
    let flag = |m: &Option<Vec<bool>>, r: usize, i: usize| {
        m.as_ref()
            .and_then(|m| m.get(r * old_cols + i))
            .copied()
            .unwrap_or(false)
    };
    let mut cells = Vec::with_capacity(head.len());
    let mut left = Vec::with_capacity(head.len() * cols);
    let mut down = Vec::with_capacity(head.len() * cols);
    for (r, row) in head.iter().enumerate() {
        let mut out = Vec::with_capacity(cols);
        for j in 0..cols {
            let i = under[j];
            let opens = j == 0 || under[j - 1] != i;
            out.push(if opens {
                row.get(i).cloned().unwrap_or_default()
            } else {
                String::new()
            });
            left.push(!opens || flag(&prev.merged_left, r, i));
            down.push(flag(&prev.merged_down, r, i));
        }
        cells.push(out);
    }
    prev.header_rows = Some(cells.len());
    prev.cells = Some(cells);
    prev.merged_left = Some(left);
    prev.merged_down = down.iter().any(|d| *d).then_some(down);
    next.continues = Some(prev.id.clone());
    next.header_rows = Some(0);
}

/// Fold a header drawn as a box of its own into the rows boxed under it.
///
/// [`link_header_box`] deals the header box's names onto the rows' columns
/// and links the rows to it, leaving the box a part of nothing but header.
/// On one page the two are one table: the box's rows become the table's
/// header rows, and the box, which held only names, goes. Left as it was, it
/// read as a table of no cells before every table so drawn.
fn fold_header_boxes(elements: &mut Vec<Element>) {
    let mut gone: Vec<usize> = Vec::new();
    for i in 0..elements.len() {
        let Some(id) = elements[i].continues.clone() else {
            continue;
        };
        let Some(j) = elements[..i].iter().rposition(|e| e.id == id) else {
            continue;
        };
        let (before, after) = elements.split_at_mut(i);
        let (head, body) = (&mut before[j], &mut after[0]);
        let (Some(hc), Some(bc)) = (head.cells.as_ref(), body.cells.as_ref()) else {
            continue;
        };
        let cols = bc.first().map_or(0, |r| r.len());
        if head.kind != "doco:Table"
            || head.page != body.page
            || hc.is_empty()
            || head.header_rows != Some(hc.len())
            || hc.iter().any(|r| r.len() != cols)
        {
            continue;
        }
        let n = hc.len();
        let flags = |e: &Option<Vec<bool>>, rows: usize| {
            e.clone().unwrap_or_else(|| vec![false; rows * cols])
        };
        let join = |a: &Option<Vec<bool>>, b: &Option<Vec<bool>>| {
            if a.is_none() && b.is_none() {
                return None;
            }
            let mut v = flags(a, n);
            v.extend(flags(b, bc.len()));
            v.iter().any(|f| *f).then_some(v)
        };
        let merged_down = join(&head.merged_down, &body.merged_down);
        let merged_left = join(&head.merged_left, &body.merged_left);
        let spread_left = join(&head.spread_left, &body.spread_left);
        let mut cells = hc.clone();
        cells.extend(bc.iter().cloned());
        let shift = |v: &Option<Vec<usize>>| v.as_ref().map(|v| v.iter().map(|r| r + n).collect());
        body.sub_headers = shift(&body.sub_headers);
        body.restated_headers = shift(&body.restated_headers);
        if let Some(d) = body.datums.as_mut() {
            let mut rows: Vec<Vec<Option<fluree_doc_model::element::Datum>>> =
                vec![vec![None; cols]; n];
            rows.append(d);
            *d = rows;
        }
        body.header_rows = Some(n + body.header_rows.unwrap_or(0));
        body.merged_down = merged_down;
        body.merged_left = merged_left;
        body.spread_left = spread_left;
        body.text = cells
            .iter()
            .map(|r| r.join(" | "))
            .collect::<Vec<_>>()
            .join("\n");
        body.cells = Some(cells);
        body.bbox = match (head.bbox, body.bbox) {
            (Some(a), Some(b)) => Some(crate::geom::BBox {
                x0: a.x0.min(b.x0),
                y0: a.y0.min(b.y0),
                x1: a.x1.max(b.x1),
                y1: a.y1.max(b.y1),
            }),
            (a, b) => b.or(a),
        };
        body.continues = head.continues.take();
        gone.push(j);
    }
    gone.sort_unstable();
    for j in gone.into_iter().rev() {
        elements.remove(j);
    }
}

/// Link rows set a little below a table, on its columns, to that table.
///
/// A run of rows read by alignment breaks where a line of something else
/// falls between two of them: a label centred on the tall row of an outer
/// table the inner one sits in (`Acoustical Performance` beside rows `1`,
/// `1.5` | `2`, `3`, `5`). The rows under the break are the same table's
/// where they follow it with nothing between, a gap of a box's spacing
/// below, on as many columns lined up with its own, and open on figures
/// with no header of their own.
fn link_rows_below(prev: &Element, next: &mut Element, a: &TableFrame, b: &TableFrame) {
    if next.continues.is_some() || a.xs.len() != b.xs.len() || a.xs.len() < 3 {
        return;
    }
    let (Some(pa), Some(pb)) = (prev.bbox, next.bbox) else {
        return;
    };
    let gap = pb.y0 - pa.y1;
    if !(-CONTINUED_EDGE_TOLERANCE..=HEADER_BOX_GAP).contains(&gap) {
        return;
    }
    // Column for column, each one's middle inside the other's: edges read
    // from the text's alignment move with each part's widths.
    let inside = |p: &[f64], q: &[f64]| {
        p.windows(2).zip(q.windows(2)).all(|(c, d)| {
            let mid = (c[0] + c[1]) / 2.0;
            mid >= d[0] && mid <= d[1]
        })
    };
    if !inside(&a.xs, &b.xs) || !inside(&b.xs, &a.xs) {
        return;
    }
    let (Some(head), Some(rows)) = (prev.cells.as_ref(), next.cells.as_ref()) else {
        return;
    };
    let headed = prev.header_rows.unwrap_or(0);
    let figures = |row: &Vec<String>| {
        let filled: Vec<&str> = row
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect();
        !filled.is_empty()
            && filled.iter().filter(|t| table::is_numeric_cell(t)).count() * 2 > filled.len()
    };
    if headed == 0 || !head.iter().skip(headed).any(figures) || !rows.first().is_some_and(figures) {
        return;
    }
    next.continues = Some(prev.id.clone());
    next.header_rows = Some(0);
}

/// Fewest bold lines stacked one after another, with nothing between them,
/// that are a list of labels rather than a section opening on its first
/// subsection.
const STACKED_LABELS: usize = 3;

/// Read headings on their weight alone that head nothing, read as text.
///
/// A section head is followed by what it heads: text, a table, a figure,
/// or a section under it. Bold lines at one level that follow each other
/// with nothing between them are looked at as a run, and these runs are
/// no sections:
///
/// - **A run that heads nothing:** the end of the document follows it, a
///   bold heading above its level, or a heading set larger, by its size or
///   as the title, which ranks above any bold line at the body's size. That
///   is the name and the role that close minutes or a letter (`Joshua
///   Gallin` over `Secretary`), a slide's kicker over its title (`LEVEL 2 ·
///   DERIVED`), or a form's label over a larger line. A label naming a part
///   of the document (`EXHIBIT B`) heads the title under it and stays, and
///   so does the document's first heading, which may be its title over a
///   larger date.
/// - **A paragraph's bold last line:** a lone bold line set under a
///   paragraph at its leading, over another bold head (`KEEP OUT OF REACH
///   OF CHILDREN` closing a caution, over `TRADEMARKS`).
/// - **A run set as a row:** two of its lines sit side by side, so it is
///   read across. That is a table's header cells left out of its grid
///   (`Date | Description`), or an answer scale (`OFTEN`, `SOMETIMES`,
///   `RARELY`, `NEVER`). Card titles set side by side on a slide are never
///   a run, because each is followed by its own text. Nor is a division's
///   label beside its title (`Part I`, then its title, on one line).
/// - **A run of three lines or more set one under another:** a list of
///   names, a chart's legend words, a form's labels. Its last line is
///   followed by text, and keeps the heading.
///
/// Two bold lines one over the other, followed by text, stay: that is a
/// section opening on its first subsection (`Materials and Methods` over
/// `Apiary selection`) as often as a label over a label.
///
/// Only weight is judged, because a numbered, outlined or larger line says
/// more than its weight does. Only bold lines and larger heads end a run: a
/// bold line's level says nothing beside a numbered or outlined heading's.
fn demote_empty_bold_heads(elements: &mut [Element]) {
    let bold = |e: &Element| e.kind == "doco:SectionTitle" && unflagged(e.evidence) == "bold";
    // A heading set larger than the body, by its size or as the title,
    // ranks above any bold line at the body's size.
    let set_larger = |e: &Element| {
        e.kind == "doco:SectionTitle" && matches!(unflagged(e.evidence), "font-size" | "title")
    };
    let level = |e: &Element| e.level.unwrap_or(6);
    let same_row = |a: &Element, b: &Element| match (a.bbox, b.bbox) {
        (Some(a_box), Some(b_box)) if a.page == b.page => {
            let overlap = a_box.y1.min(b_box.y1) - a_box.y0.max(b_box.y0);
            let shorter = (a_box.y1 - a_box.y0).min(b_box.y1 - b_box.y0);
            shorter > 0.0 && overlap >= shorter * 0.5
        }
        _ => false,
    };
    // The document's first heading may be its title, set bold over a
    // larger date or subtitle (`Transcript of Chair Powell's Press
    // Conference` over `April 29, 2026`).
    let first_heading = elements.iter().position(|e| e.kind == "doco:SectionTitle");
    let mut demote: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < elements.len() {
        if !bold(&elements[i]) {
            i += 1;
            continue;
        }
        // A bold line set at its paragraph's leading under it, over another
        // bold head: the paragraph's last line, in bold (`KEEP OUT OF REACH
        // OF CHILDREN` closing a caution, over `TRADEMARKS`). The run is
        // judged from the head after it. A caption closes nothing: what
        // follows it is the document's next part (`Table 3 Summary of…`,
        // then a species' heading).
        if i > 0
            && elements[i - 1].kind == "doco:Paragraph"
            && !heading::is_caption(&elements[i - 1].text)
            && under_at_leading(&elements[i - 1], &elements[i])
            && elements.get(i + 1).is_some_and(bold)
        {
            demote.push(i);
            i += 1;
            continue;
        }
        let lvl = level(&elements[i]);
        let mut j = i;
        while j + 1 < elements.len() && bold(&elements[j + 1]) && level(&elements[j + 1]) == lvl {
            j += 1;
        }
        let heads_nothing = match elements.get(j + 1) {
            None => true,
            Some(next) => {
                (bold(next) && level(next) < lvl)
                    || (set_larger(next)
                        && !designated_label(&elements[j].text)
                        && Some(i) != first_heading)
            }
        };

        // A division's label beside its title (`Part I | Annual and Monthly
        // Contribution Amount`) is one heading set in two blocks, not a row.
        let row = (i + 1..=j).any(|k| {
            same_row(&elements[k - 1], &elements[k])
                && !heading::division_label(&elements[k - 1].text)
        });
        if heads_nothing || row {
            demote.extend(i..=j);
        } else if j + 1 - i >= STACKED_LABELS {
            demote.extend(i..j);
        }
        i = j + 1;
    }
    for i in demote {
        let e = &mut elements[i];
        e.kind = "doco:Paragraph".into();
        e.level = None;
        e.evidence = "layout";
    }
}

/// `b` stands under `a`, sharing some of its width, at no more than a
/// line's spacing: no gap opens between them beyond the leading of `b`'s
/// own line.
fn under_at_leading(a: &Element, b: &Element) -> bool {
    match (a.bbox, b.bbox) {
        (Some(a_box), Some(b_box)) if a.page == b.page => {
            let height = (b_box.y1 - b_box.y0).max(1.0);
            let gap = b_box.y0 - a_box.y1;
            let shared = a_box.x1.min(b_box.x1) > a_box.x0.max(b_box.x0);
            shared && gap >= -height * 0.2 && gap <= height * CLOSING_LINE_GAP
        }
        _ => false,
    }
}

/// Widest gap, in the line's own heights, between a paragraph and a bold
/// line that closes it: the leading's share of a line, short of the space
/// a heading takes above it.
const CLOSING_LINE_GAP: f64 = 0.5;

/// How many of its own line heights above a table's top a heading set
/// beside it, to its left, may stand and still head it.
const BESIDE_HEADING_LINES: f64 = 3.0;

/// The heading a table sits under, by its index in `content`: the nearest
/// heading above the table's top that is in its lane (overlapping it across)
/// or set beside it, to its left, level with its top: within
/// [`BESIDE_HEADING_LINES`] of its own height above it, or set on the
/// table's first row, its top within its own height of the table's. A
/// submittal sets its side heads in a column of their own, each level with
/// the top of what it heads (`TEST DATA` beside the test-data table's
/// header row). A head on the first row heads the table only where no other
/// head stands beside its later rows: `Theory`, `Practice` and `Values`
/// down the left of a table are its rows' labels, its label column unread.
fn own_heading(content: &[Element], table: &Element) -> Option<usize> {
    let t = table.rect();
    let heads = || {
        content
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == "doco:SectionTitle" && e.page == table.page)
    };
    let row_labels = |h: &crate::geom::BBox| {
        heads().any(|(_, o)| {
            let o = o.rect();
            o.x1 <= t.x0 && o.y0 > h.y1 && o.y0 < t.y1
        })
    };
    heads()
        .filter(|(_, e)| {
            let h = e.rect();
            let line = h.y1 - h.y0;
            let lane = h.x1.min(t.x1) > h.x0.max(t.x0) && h.y1 <= t.y0 + 1.0;
            let above = h.y1 <= t.y0 + 1.0;
            let beside = h.x1 <= t.x0
                && h.y0 <= t.y0 + line
                && t.y0 - h.y1 <= BESIDE_HEADING_LINES * line
                && (above || !row_labels(&h));
            lane || beside
        })
        .max_by(|(_, a), (_, b)| a.rect().y1.total_cmp(&b.rect().y1))
        .map(|(i, _)| i)
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
        // A table set inside another's box, a grid nested in one of its
        // cells, follows it, after any others nested in it before.
        let within = |inner: &Element, host: &Element| {
            let (h, t) = (host.rect(), inner.rect());
            host.kind == "doco:Table"
                && inner.kind == "doco:Table"
                && t.x0 >= h.x0 - 1.0
                && t.x1 <= h.x1 + 1.0
                && t.y0 >= h.y0 - 1.0
                && t.y1 <= h.y1 + 1.0
        };
        if let Some(host) = content.iter().position(|e| within(&table, e)) {
            let mut at = host + 1;
            while at < content.len() && within(&content[at], &content[host]) {
                at += 1;
            }
            content.insert(at, table);
            continue;
        }
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
        let mut insertion = last_before
            .map(|i| i + 1)
            .or(first_after)
            .unwrap_or_else(|| {
                content
                    .iter()
                    .position(|e| e.rect().y0 >= table.rect().y1)
                    .unwrap_or(content.len())
            });
        // A table is in the section of the heading it sits under, or beside:
        // never ahead of it. A catalogue sets each product's heading at the
        // left over its photo and its table to the right, the heading level
        // with the table's top; the table overlaps no block of its own
        // section above it, only the last product's, and went there.
        if let Some(h) = own_heading(&content, &table).filter(|&h| insertion <= h) {
            insertion = h + 1;
            // Past what is set under the heading above the table's top too:
            // its note (`UPS option`) beside the table's first rows. And past
            // the matter of a column read before the table's own, up to the
            // first text in its lane: two tables side by side under one
            // heading, each over its note, read the left table and its note
            // before the right table and its note.
            let own_next = first_after.filter(|&f| f >= insertion);
            let t = table.rect();
            let in_lane = |e: &Element| e.rect().x1.min(t.x1) > e.rect().x0.max(t.x0);
            while insertion < content.len()
                && content[insertion].kind != "doco:SectionTitle"
                && (content[insertion].rect().y1 <= t.y0
                    || own_next.is_some_and(|f| insertion < f && !in_lane(&content[insertion])))
            {
                insertion += 1;
            }
        }
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

/// Share of a ruled grid's cells that must hold text for it to be a table
/// drawn over a drawing, rather than the drawing's own lines around its
/// labels.
const RULED_TABLE_FILL: f64 = 0.5;

/// How far, as a share of the page's height, a block may stand from a
/// running line's row and still be on it: a baseline's wobble between pages.
const FURNITURE_ROW_SLACK: f64 = 0.005;

/// Fraction of a block that must lie inside a figure's drawing for the
/// block to belong to it without being taken as a label. High enough that a
/// paragraph brushing the drawing's margin stays prose.
const FIGURE_CONTAINMENT: f64 = 0.6;

/// Most words a figure's label says; a block of the page's prose inside a
/// figure's region says more (see [`find_figures`]).
const FIGURE_PROSE_WORDS: usize = 15;

/// Share of the body's size a paragraph inside a figure's region may differ
/// by and still be the page's prose.
const FIGURE_PROSE_SIZE: f64 = 0.12;

/// Fraction of a grid inferred from its text's alignment, and corroborated by
/// drawn geometry, that must lie on a drawing for its alignment to be the
/// drawing's.
const ALIGNED_IN_DRAWING: f64 = 0.5;

/// How many times its own boundaries the distinct places the strokes inside
/// a ruled grid stand at may run to, each way, before they are a drawing's.
const LINE_ART_STROKES: usize = 2;

/// Fewest of a grid's rules that end in an arrowhead for it to be a
/// dimension drawing's: one dimension line, arrowed at both ends, or one
/// broken around its callout.
const DIMENSION_ARROWS: usize = 2;

/// Largest arrowhead across its line, in the grid's text size: a drawing's
/// arrows are drawn to its callouts' scale.
const ARROWHEAD_ACROSS: f64 = 1.0;

/// Largest arrowhead along its line, in the grid's text size.
const ARROWHEAD_ALONG: f64 = 1.5;

/// Words a dimension drawing sets beside its figures: units, and the notes
/// a dimension carries (`TYP`, `MAX`).
const DIMENSION_WORDS: &[&str] = &["mm", "cm", "in", "ft", "typ", "max", "min", "ref"];

/// Is `g` a dimension drawing's? Its rules are dimension and extension
/// lines, [`DIMENSION_ARROWS`] or more ending in an arrowhead (a small fill
/// on the line's end, longer along it than across), and its cells hold
/// callouts alone: figures with their units (`83⁵/16"`, `(2117 mm)`), counts
/// (`6X`) and datum letters, no words. Read as a table, its callouts are
/// cells of a grid its lines happen to make. A table's rules end at its
/// edges and its cells hold words, a header at least.
fn dimension_drawing(g: &Grid, page: &crate::extract::Page, cells: &[String]) -> bool {
    if !callouts(cells) {
        return false;
    }
    let mut sizes: Vec<f32> = page
        .glyphs
        .iter()
        .filter(|gl| {
            gl.bbox
                .is_some_and(|b| figure::share_inside(&b, &g.bbox) >= 0.5)
        })
        .map(|gl| gl.font_size)
        .collect();
    sizes.sort_by(f32::total_cmp);
    let Some(&em) = sizes.get(sizes.len() / 2) else {
        return false;
    };
    let em = em.max(1.0) as f64;
    let inside = |b: &crate::geom::BBox| {
        b.x0 >= g.bbox.x0 - 3.0
            && b.x1 <= g.bbox.x1 + 3.0
            && b.y0 >= g.bbox.y0 - 3.0
            && b.y1 <= g.bbox.y1 + 3.0
    };
    let arrowed = page
        .rules
        .iter()
        .filter(|r| inside(&r.bbox))
        .filter(|r| ends_in_arrowhead(r, &page.fills, em))
        .count();
    arrowed >= DIMENSION_ARROWS
}

/// Does the rule end in an arrowhead: a small fill, no larger than
/// [`ARROWHEAD_ACROSS`] and [`ARROWHEAD_ALONG`] of the text size `em`,
/// longer along the line than across it, set on its axis over one of its
/// ends? A line drawn to the arrow's tip or to its base ends inside it.
fn ends_in_arrowhead(r: &crate::rule::Rule, fills: &[crate::rule::Fill], em: f64) -> bool {
    let b = r.bbox;
    let vertical = r.orientation == crate::rule::Orientation::Vertical;
    let (axis, ends) = if vertical {
        ((b.x0 + b.x1) * 0.5, [b.y0, b.y1])
    } else {
        ((b.y0 + b.y1) * 0.5, [b.x0, b.x1])
    };
    fills.iter().any(|f| {
        let fb = f.bbox;
        let (across, along) = if vertical {
            ((fb.x0, fb.x1), (fb.y0, fb.y1))
        } else {
            ((fb.y0, fb.y1), (fb.x0, fb.x1))
        };
        let (wide, long) = (across.1 - across.0, along.1 - along.0);
        wide <= em * ARROWHEAD_ACROSS
            && long <= em * ARROWHEAD_ALONG
            && long >= wide
            && across.0 <= axis
            && across.1 >= axis
            && ends
                .iter()
                .any(|&e| e >= along.0 - 1.0 && e <= along.1 + 1.0)
    })
}

/// Do the cells hold callouts alone: some figures, and no word but a unit or
/// a dimension's note? A count's `X` or a datum's letter stands alone.
fn callouts(cells: &[String]) -> bool {
    let mut figures = false;
    for token in cells.iter().flat_map(|c| c.split_whitespace()) {
        figures |= token.chars().any(char::is_numeric);
        let worded = token
            .split(|c: char| !c.is_alphabetic())
            .filter(|w| w.chars().count() >= 2)
            .any(|w| !DIMENSION_WORDS.contains(&w.to_lowercase().as_str()));
        if worded {
            return false;
        }
    }
    figures
}

/// Are the rules inside `g` a drawing's? Its vertical strokes stand at more
/// than [`LINE_ART_STROKES`] times its column boundaries' count of places
/// across it, and its horizontal ones at more than as many times its row
/// boundaries' down it. Places are counted to the point, so a rule drawn in
/// dashes, or cell by cell, is one.
/// Farthest a tick, or a gridline's end, stands from the side of its
/// number, in the number's size.
const TICK_REACH: f64 = 1.5;

/// Farthest a tick's middle stands above or below its number's, in the
/// number's size: level with it, where an underline runs under it.
const TICK_LEVEL: f64 = 0.3;

/// Is grid `g` a chart's scale beside the page's prose rather than a
/// table? A column of two numbers or more, each level with a horizontal
/// stroke that ends beside it (an axis's tick, or a gridline's end), with
/// at most one other cell, the axis's unit (`Percent`); one column of
/// running prose, the page's column read across the gutter with the
/// chart's scale (`spending have deteriorated. Business | 18.0`); and
/// beyond them only columns of a word or a few, the chart's own labels. A
/// table's figures stand between its rules, not level with a stroke's end,
/// and a table holds columns of text of its own.
fn scale_beside_text(
    g: &Grid,
    glyphs: &[crate::glyph::Glyph],
    rules: &[crate::rule::Rule],
    ticks: &[crate::rule::Rule],
) -> bool {
    let (rows, cols) = (g.rows(), g.cols());
    if rows < 2 || cols < 2 {
        return false;
    }
    let texts = g.cell_texts(glyphs);
    let homes = g.glyph_cells(glyphs);
    // Each cell's ink and its text's size.
    let mut boxes: Vec<Option<(crate::geom::BBox, f64)>> = vec![None; rows * cols];
    for (gl, home) in glyphs.iter().zip(&homes) {
        let (Some(i), Some(b)) = (home, gl.bbox) else {
            continue;
        };
        if gl.text.trim().is_empty() {
            continue;
        }
        let size = (gl.font_size as f64).max(1.0);
        boxes[*i] = Some(match boxes[*i] {
            Some((u, s)) => (u.union(&b), s.max(size)),
            None => (b, size),
        });
    }
    let number = |t: &str| {
        let t = t.trim();
        !t.is_empty()
            && t.chars().count() <= 9
            && t.chars().any(|c| c.is_ascii_digit())
            && t.chars()
                .all(|c| c.is_ascii_digit() || ".,−-+%".contains(c))
    };
    let horizontal = rules
        .iter()
        .chain(ticks)
        .filter(|r| r.orientation == crate::rule::Orientation::Horizontal);
    let ticks: Vec<crate::geom::BBox> = horizontal.map(|r| r.bbox).collect();
    let ticked = |b: &crate::geom::BBox, size: f64| {
        let y = (b.y0 + b.y1) / 2.0;
        ticks.iter().any(|t| {
            let ty = (t.y0 + t.y1) / 2.0;
            let left = t.x1 <= b.x0 + 1.0 && b.x0 - t.x1 <= size * TICK_REACH;
            let right = t.x0 >= b.x1 - 1.0 && t.x0 - b.x1 <= size * TICK_REACH;
            (ty - y).abs() <= size * TICK_LEVEL && (left || right)
        })
    };
    // Lines of running text, a cell each: words enough to a line, and a
    // line that carries on into the next (`… Business` over `investment
    // rose …`). A chart's labels set beside its scale say a word or two,
    // and stay with it.
    let prose = |filled: &[usize]| {
        let lines: Vec<&str> = filled.iter().map(|&i| texts[i].trim()).collect();
        let worded = lines
            .iter()
            .filter(|t| t.split_whitespace().count() >= 4)
            .count();
        let runs_on = lines.windows(2).any(|w| {
            let ends_open = !w[0].ends_with(['.', '!', '?', ':', ';']);
            let next = w[1].split_whitespace().next().unwrap_or("");
            ends_open
                && w[0].split_whitespace().count() >= 4
                && next.chars().next().is_some_and(char::is_lowercase)
        });
        worded >= 2 && runs_on
    };
    // Columns of the chart's own scale, of running prose, and of other
    // text; a column of a word or a few to a cell is the chart's legend
    // or a series' name (`Yield`), set inside the plot.
    let mut scales = 0;
    let mut texted = 0;
    let mut prosed = 0;
    for c in 0..cols {
        let filled: Vec<usize> = (0..rows)
            .map(|r| r * cols + c)
            .filter(|&i| !texts[i].trim().is_empty())
            .collect();
        if filled.is_empty() {
            continue;
        }
        let numbers: Vec<usize> = filled
            .iter()
            .copied()
            .filter(|&i| number(&texts[i]))
            .collect();
        let on_ticks = numbers
            .iter()
            .filter(|&&i| boxes[i].is_some_and(|(b, size)| ticked(&b, size)))
            .count();
        if numbers.len() >= 2 && on_ticks == numbers.len() && numbers.len() + 1 >= filled.len() {
            scales += 1;
        } else if prose(&filled) {
            prosed += 1;
        } else if filled
            .iter()
            .any(|&i| texts[i].split_whitespace().count() > 3)
        {
            texted += 1;
        }
    }
    scales >= 1 && prosed == 1 && texted == 0
}

fn line_art(g: &Grid, rules: &[crate::rule::Rule]) -> bool {
    let inside = |r: &&crate::rule::Rule| {
        r.bbox.x0 >= g.bbox.x0 - 3.0
            && r.bbox.x1 <= g.bbox.x1 + 3.0
            && r.bbox.y0 >= g.bbox.y0 - 3.0
            && r.bbox.y1 <= g.bbox.y1 + 3.0
    };
    let places = |vertical: bool| {
        let mut at: Vec<i64> = rules
            .iter()
            .filter(inside)
            .filter(|r| matches!(r.orientation, crate::rule::Orientation::Vertical) == vertical)
            .map(|r| {
                let mid = if vertical {
                    (r.bbox.x0 + r.bbox.x1) * 0.5
                } else {
                    (r.bbox.y0 + r.bbox.y1) * 0.5
                };
                mid.round() as i64
            })
            .collect();
        at.sort_unstable();
        at.dedup_by(|a, b| (*a - *b).abs() <= 1);
        at.len()
    };
    places(true) > LINE_ART_STROKES * (g.cols() + 1)
        && places(false) > LINE_ART_STROKES * (g.rows() + 1)
}

/// Lines of a scale's text, at most, between an axis's scale and the plot
/// it measures: the space ticks and their labels take, as the axis test of
/// a scatter's values allows (see `figure::axis`).
const SCALE_REACH: f64 = 4.0;

/// The number a scale's label says: a minus may be set as a true minus or
/// an en dash, and thousands are grouped with commas (`\u{2212}0.5`,
/// `2,000`).
pub(crate) fn axis_value(token: &str) -> Option<f64> {
    token
        .replace(['\u{2212}', '\u{2013}'], "-")
        .replace(',', "")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Do the cells, read in order, say nothing but numbers that step evenly:
/// three or more, by one difference (`160 140 120 … 0`) or one ratio, as a
/// logarithmic axis steps (`1 10 100 1,000`)? A plot's scale, its tick
/// marks drawn at each number, reads as a ruled row or column; a table's
/// column of figures does not step so, or stands beside its labels.
pub(crate) fn axis_scale(cells: &[String]) -> bool {
    let mut values: Vec<f64> = Vec::new();
    for token in cells.iter().flat_map(|c| c.split_whitespace()) {
        match axis_value(token) {
            Some(v) => values.push(v),
            None => return false,
        }
    }
    if values.len() < 3 {
        return false;
    }
    let even = |steps: Vec<f64>| {
        let first = steps[0];
        first.abs() > f64::EPSILON
            && steps
                .iter()
                .all(|s| (s - first).abs() <= first.abs() * 0.01)
    };
    let differences: Vec<f64> = values.windows(2).map(|w| w[1] - w[0]).collect();
    let ratios: Option<Vec<f64>> = values
        .windows(2)
        .map(|w| (w[0].abs() > f64::EPSILON).then(|| w[1] / w[0]))
        .collect();
    even(differences) || ratios.is_some_and(|r| r[0] > 0.0 && (r[0] - 1.0).abs() > 0.01 && even(r))
}

/// Fewest numbers a grid's column holds for it to be read as a chart's scale.
const SCALE_VALUES: usize = 3;

/// Fewest words in a line of prose, as a cell reads one.
const PROSE_WORDS: usize = 5;

/// Least share of a line's words set in lower case for it to read as prose:
/// a chart's titles and legends are capitalised, figured or short (`All
/// needs met by 2020`), a paragraph's line runs on in lower case (`remains
/// much further below its pre-pandemic level`).
const PROSE_LOWER: f64 = 0.6;

/// Does the cell read as a line of prose?
pub(crate) fn prose_line(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower = words
        .iter()
        .filter(|w| w.chars().next().is_some_and(char::is_lowercase))
        .count();
    words.len() >= PROSE_WORDS && lower as f64 >= words.len() as f64 * PROSE_LOWER
}

/// A cell's text as one number, a minus or a dash for its sign and commas
/// for thousands: `−20`, `1,000`.
fn scale_number(text: &str) -> Option<f64> {
    let t = text
        .trim()
        .replace(['\u{2212}', '\u{2013}'], "-")
        .replace(',', "");
    (!t.is_empty()).then(|| t.parse::<f64>().ok()).flatten()
}

/// The chart a grid of axes belongs to, where charts set side by side line
/// their scales up into one grid (`240 | 240` down to `0 | 0`, each panel's
/// title over its scale). Every column holding numbers reads as a scale:
/// [`SCALE_VALUES`] or more stepping evenly ([`axis_scale`]), each beside a
/// chart's drawing, level with half its height or more and within an axis's
/// reach ([`SCALE_REACH`] of the grid's text size `line`). Cells of words, a
/// panel's title or legend, say nothing either way. A table's columns of
/// figures step unevenly; one whose numbers do (a rank, a conversion) stands
/// beside no chart. The nearest chart takes the grid's text.
fn panel_axes(g: &Grid, cells: &[String], figures: &[figure::Figure], line: f64) -> Option<usize> {
    // A line of prose among the cells is the page's text beside a chart,
    // read into the grid with its scale: the grid is no chart's axes.
    if cells.iter().any(|c| prose_line(c)) {
        return None;
    }
    let cols = g.cols();
    let mut nearest: Option<(usize, f64)> = None;
    for c in 0..cols {
        let rows: Vec<usize> = (0..g.rows())
            .filter(|&r| scale_number(&cells[r * cols + c]).is_some())
            .collect();
        if rows.is_empty() {
            continue;
        }
        let values: Vec<String> = rows.iter().map(|&r| cells[r * cols + c].clone()).collect();
        if rows.len() < SCALE_VALUES || !axis_scale(&values) {
            return None;
        }
        let (x0, x1) = (g.xs[c], g.xs[c + 1]);
        let (y0, y1) = (g.ys[rows[0]], g.ys[rows[rows.len() - 1] + 1]);
        let beside = figures
            .iter()
            .enumerate()
            .filter_map(|(fi, f)| {
                let d = f.drawing;
                let level = y1.min(d.y1) - y0.max(d.y0) >= 0.5 * (y1 - y0);
                let gap = (d.x0 - x1).max(x0 - d.x1).max(0.0);
                (level && gap <= line * SCALE_REACH).then_some((fi, gap))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        if nearest.is_none_or(|(_, gap)| beside.1 < gap) {
            nearest = Some(beside);
        }
    }
    nearest.map(|(fi, _)| fi)
}

/// Does a heading give way to the figure it is in? Relative typography does
/// wherever the figure took it: chart titles, legends and axis labels are
/// set larger or bolder than body text. A number or a place in a sequence
/// does only inside the drawing itself, where `4-M4` is a dimension's
/// callout and not a section; a numbered heading the figure took from
/// beside it keeps its number's evidence. A title and the outline never do.
fn heading_yields_to_figure(evidence: Evidence, in_figure: bool, in_drawing: bool) -> bool {
    match evidence {
        Evidence::FontSize | Evidence::Bold => in_figure,
        Evidence::Numbering | Evidence::Sequence => in_drawing,
        Evidence::Title | Evidence::Outline => false,
    }
}

/// Evidence a heading set as a form's text carries until runs of bold
/// heads are judged: the evidence it would have, with this prefix.
const FORM_TEXT: &str = "form-";

/// A heading's evidence, without the mark of a form's text.
fn unflagged(evidence: &'static str) -> &'static str {
    evidence.strip_prefix(FORM_TEXT).unwrap_or(evidence)
}

/// A form's text, kept a heading while runs of bold heads were judged
/// ([`heading::Heading::form_text`]), is read as text from here on.
fn read_form_text(elements: &mut [Element]) {
    for e in elements.iter_mut() {
        if e.kind == "doco:SectionTitle" && e.evidence.starts_with(FORM_TEXT) {
            e.kind = "doco:Paragraph".into();
            e.level = None;
            e.evidence = "layout";
        }
    }
}

/// Least an item's marker stands right of the marker before it, in its
/// size, to be set under that item: less is the same list's marker drawn a
/// little off, or a wider number right-aligned.
const NEST_INDENT: f64 = 1.0;

/// Farthest before where the item before it starts its text, in its size,
/// an item's marker stands set under that item: a slide's second-level
/// bullet stands a little left of the first level's text.
const NEST_BEFORE: f64 = 1.0;

/// Farthest past where the item before it starts its text, in its size, an
/// item's marker stands set under that item. A sub-list's marker stands
/// under its parent's text, or a little into it; further is another column.
const NEST_REACH: f64 = 2.5;

/// Most space between one item and the next, in their size, for them to be
/// read as one list: a list set with a blank line between its items.
const LIST_GAP: f64 = 3.0;

/// A list item's marker and text start, its size, whether its first line
/// carries on the item before it, and whether it is marked by a box to tick.
type ListMark = (f64, f64, f64, bool, bool);

/// A page's list items, nested by where their markers stand.
///
/// An item whose marker stands under the text of the item before it, past
/// that item's own marker, is set in a list inside it: a bulleted
/// `UL Listed to control:` with its dashed loads indented under its text.
/// An item whose marker stands back out returns to the level its marker
/// stands at. Only an item following the one before it down the page, with
/// a marker of its own, nests: a run broken by anything else, or carried to
/// the next column, starts again at the top. A wrapped line is its item's
/// own, and a paragraph set with a hanging indent has no marker. A line
/// read as an item though it carries on the one before, its count standing
/// beside a later line, is not set under it. A box to tick marks a choice,
/// not what a list is set under: a form's `Yes` and `No` boxes, and what is
/// set beside or under them, stay level.
fn nest_list_items(elements: &mut [Element], item_text: &[Option<ListMark>]) {
    // Each open level's marker and text start, and whether it is a box to
    // tick, outermost first.
    let mut open: Vec<(f64, f64, bool)> = Vec::new();
    let mut prev: Option<crate::geom::BBox> = None;
    for (e, t) in elements.iter_mut().zip(item_text) {
        let (Some((marker, text_x, size, carried_on, ticked)), Some(b)) = (*t, e.bbox) else {
            open.clear();
            prev = None;
            continue;
        };
        let em = size.max(1.0);
        let follows = prev.is_some_and(|p| b.y0 >= p.y0 && b.y0 - p.y1 <= LIST_GAP * em);
        if !follows {
            open.clear();
        }
        match open.last().copied() {
            None => open.push((marker, text_x, ticked)),
            Some((above, above_text, above_ticked))
                if !carried_on
                    && !above_ticked
                    && marker >= above + NEST_INDENT * em
                    && marker >= above_text - NEST_BEFORE * em
                    && marker <= above_text + NEST_REACH * em =>
            {
                open.push((marker, text_x, ticked))
            }
            Some(_) => {
                while open.len() > 1 && open.last().is_some_and(|l| marker < l.0 - 0.5 * em) {
                    open.pop();
                }
                if let Some(level) = open.last_mut() {
                    *level = (marker, text_x, ticked);
                }
            }
        }
        e.depth = open.len() - 1;
        prev = Some(b);
    }
}

/// Most words in a table's column head left out above it: `2008 vs. 2007
/// 2007 vs. 2006`, two heads read as one line. A title runs longer.
const MAX_TABLE_HEAD_WORDS: usize = 8;

/// A table's column head left out of the table, read as a heading on its
/// weight or size: a short line set on the table's top edge, within a line's
/// height of it with nothing between, inside its width, neither from its
/// left edge nor centred on it, and over its value columns (past its
/// middle) or on a row of two or more of the table's other heads. `Total
/// SOMA` over its two years' columns, `Item` over the stub beside the period heads, `2008 vs. 2007
/// 2007 vs. 2006` over the change columns. A table's title is set from its
/// left edge or centred over it, and a section's heading over a table
/// starts where the text does. Run on the final elements, where the
/// tables are.
/// Fewest entries in a run of headings, each over one short line, for the
/// run to be a list.
const MIN_LIST_ENTRIES: usize = 3;

/// Most words in an entry's own heading, and in the short line under it.
const MAX_ENTRY_WORDS: usize = 6;
const MAX_ENTRY_NOTE_WORDS: usize = 3;

/// A list set as headings: a run of three or more headings of one level on
/// a page, each over one short line and nothing else (`Boston` over `First
/// District`, `New York` over `Second District`, …). A list of links, each
/// with its second label; a section heads more than a line of a few words.
fn read_entry_lists(elements: &mut [Element]) {
    let entry = |e: &Element| {
        e.kind == "doco:SectionTitle"
            && matches!(e.evidence, "bold" | "font-size")
            && e.text.split_whitespace().count() <= MAX_ENTRY_WORDS
    };
    let note = |e: &Element| {
        e.kind == "doco:Paragraph" && e.text.split_whitespace().count() <= MAX_ENTRY_NOTE_WORDS
    };
    let mut i = 0;
    while i < elements.len() {
        let mut heads = Vec::new();
        let mut k = i;
        while k + 1 < elements.len()
            && entry(&elements[k])
            && note(&elements[k + 1])
            && elements[k].page == elements[i].page
            && elements[k].level == elements[i].level
        {
            heads.push(k);
            k += 2;
        }
        if heads.len() >= MIN_LIST_ENTRIES {
            for h in heads {
                let e = &mut elements[h];
                e.kind = "doco:Paragraph".into();
                e.level = None;
                e.evidence = "layout";
            }
            i = k;
        } else {
            i += 1;
        }
    }
}

/// Most words in a contents entry's title set between its label and its
/// page's number.
const MAX_ENTRY_TITLE_WORDS: usize = 12;

/// Fewest page numbers ending rows at one right edge for the rows to be a
/// contents' entries.
const MIN_CONTENTS_ROWS: usize = 3;

/// A contents' entry set as a heading: its row ends on a page's number,
/// one of three or more on the page with nothing after them on their rows,
/// running in order down the page (`PART IX` and `Repeals, modifications
/// and revocations` beside `80`).
/// A page that lists its contents without leaders or a `Contents` heading
/// of its own, as a contents' later pages do, sets each number after its
/// entry's title or at one right edge.
/// Words a heading closes on to say it continues a section begun earlier,
/// lowercase: `(continued)`, `—continued`, `- Continued`, `(cont'd)`,
/// `(suite)`, `(Fortsetzung)`, `(continuación)`.
const CONTINUED_WORDS: &[&str] = &[
    "continued",
    "cont",
    "cont.",
    "cont'd",
    "cont’d",
    "contd",
    "contd.",
    "suite",
    "fortsetzung",
    "forts.",
    "continuación",
    "continuacion",
    "continua",
    "continuação",
];

/// The heading without its closing continuation mark, where it has one:
/// `Box 1—continued` is `Box 1`, `5.5 Electrical Characteristics
/// (continued)` is `5.5 Electrical Characteristics`. The mark closes the
/// line after a dash, a parenthesis or a comma, or stands alone after
/// words.
fn continued_from(text: &str) -> Option<&str> {
    let t = text.trim_end();
    let inner = t.strip_suffix(')').unwrap_or(t);
    let cut =
        inner.rfind(|c: char| matches!(c, '(' | '—' | '–' | '-' | ',') || c.is_whitespace())?;
    let word = inner[cut..]
        .trim_start_matches(|c: char| matches!(c, '(' | '—' | '–' | '-' | ',') || c.is_whitespace())
        .to_lowercase();
    if !CONTINUED_WORDS.contains(&word.as_str()) {
        return None;
    }
    let base = inner[..cut]
        .trim_end_matches(|c: char| matches!(c, '(' | '—' | '–' | '-' | ',') || c.is_whitespace());
    (base.chars().any(char::is_alphanumeric)).then_some(base)
}

/// Fewest letters and figures in what a continued heading restates.
const MIN_CONTINUED_KEY: usize = 4;

/// Letters and figures, lowercase: two headings name one section where
/// these agree.
fn heading_key(t: &str) -> String {
    t.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A heading that continues a section begun on an earlier page is no new
/// section: its text runs on in the section it continues.
///
/// - **Marked:** it restates an earlier heading, or a part of one, and
///   closes on a continuation mark (`Box 1—continued` after `Box 1.
///   Employment and Earnings across Demographic Groups`, `Developments
///   Related to Financial Stability (continued)` after `Box 3. Developments
///   Related to Financial Stability`, `Wiring Diagrams (continued)`).
/// - **Repeated:** it opens its page and repeats, word for word, the
///   nearest heading at its level or above on an earlier page: a page's
///   title set over each page a section fills (`PACKAGE OPTION ADDENDUM`),
///   a release line over each page of a table set.
fn read_continued_heads(elements: &mut [Element]) {
    let title = |e: &Element| e.kind == "doco:SectionTitle";
    let mut demote = Vec::new();
    for i in 0..elements.len() {
        if !title(&elements[i]) {
            continue;
        }
        let e = &elements[i];
        if let Some(base) = continued_from(&e.text) {
            let key = heading_key(base);
            if key.chars().count() >= MIN_CONTINUED_KEY
                && elements[..i].iter().filter(|o| title(o)).any(|o| {
                    continued_from(&o.text).is_none() && heading_key(&o.text).contains(&key)
                })
            {
                demote.push(i);
            }
            continue;
        }
        // The nearest heading at its level or above, those already read as
        // continuing aside.
        let level = e.level.unwrap_or(6);
        let nearest = (0..i)
            .rev()
            .filter(|k| title(&elements[*k]) && !demote.contains(k))
            .map(|k| &elements[k])
            .find(|o| o.level.unwrap_or(6) <= level);
        // And it opens its page's body, a continued heading or another
        // before it at most: a section begun on its page under a name an
        // earlier one shares (a handbook's templates, each titled
        // `IMPORTANT INFORMATION ABOUT YOUR DRINKING WATER` under its own
        // label) is a section of its own.
        let opens_page = (0..i)
            .rev()
            .take_while(|k| elements[*k].page == e.page)
            .all(|k| demote.contains(&k));
        if let Some(o) = nearest {
            if opens_page && o.page < e.page && heading_key(&o.text) == heading_key(&e.text) {
                demote.push(i);
            }
        }
    }
    for i in demote {
        let e = &mut elements[i];
        e.kind = "doco:Paragraph".into();
        e.level = None;
        e.evidence = "layout";
    }
}

fn read_contents_rows(elements: &mut [Element]) {
    let folio = |t: &str| {
        let t = t.trim();
        (1..=4).contains(&t.len()) && t.chars().all(|c| c.is_ascii_digit())
    };
    let pages: std::collections::BTreeSet<usize> = elements.iter().map(|e| e.page).collect();
    for page in pages {
        let on_page: Vec<usize> = (0..elements.len())
            .filter(|&i| elements[i].page == page)
            .collect();
        let row = |a: &crate::geom::BBox, b: &crate::geom::BBox| {
            a.y1.min(b.y1) - a.y0.max(b.y0) > a.height().min(b.height()) * 0.5
        };
        // Page numbers ending their rows.
        let numbers: Vec<crate::geom::BBox> = on_page
            .iter()
            .filter_map(|&i| {
                let e = &elements[i];
                let b = e.bbox?;
                (e.kind == "doco:Paragraph" && folio(&e.text)).then_some(b)
            })
            .filter(|b| {
                !on_page.iter().any(|&o| {
                    elements[o]
                        .bbox
                        .is_some_and(|ob| row(&ob, b) && ob.x0 >= b.x1)
                })
            })
            .collect();
        if numbers.len() < MIN_CONTENTS_ROWS {
            continue;
        }
        // A contents' pages run in order down the page, most of them
        // different. A catalogue's quantities (`7`, `7`, `1`, `2`, or `1`,
        // `1`, `4`) or a chart's scale (`7000` over `3500`) do not.
        let mut by_height: Vec<(f64, u32)> = on_page
            .iter()
            .filter_map(|&i| {
                let e = &elements[i];
                let b = e.bbox?;
                let n: u32 = e.text.trim().parse().ok()?;
                numbers.iter().any(|m| m == &b).then_some((b.y0, n))
            })
            .collect();
        by_height.sort_by(|a, b| a.0.total_cmp(&b.0));
        let distinct: std::collections::BTreeSet<u32> = by_height.iter().map(|p| p.1).collect();
        if by_height.windows(2).any(|w| w[1].1 < w[0].1)
            || distinct.len() < MIN_CONTENTS_ROWS
            || distinct.len() * 3 < by_height.len() * 2
        {
            continue;
        }
        let aligned = numbers;
        for &i in &on_page {
            let e = &elements[i];
            let Some(b) = e.bbox else { continue };
            // Only the entry's own short text between it and its number: a
            // slide's title level with a chart's scale has the slide's
            // bullets between.
            let entry_row = |n: &crate::geom::BBox| {
                row(&b, n)
                    && n.x0 >= b.x1
                    && !on_page.iter().any(|&o| {
                        let other = &elements[o];
                        other.bbox.is_some_and(|ob| {
                            row(&ob, &b)
                                && ob.x0 >= b.x1
                                && ob.x1 <= n.x0
                                && (other.kind == "doc:ListItem"
                                    || other.text.split_whitespace().count()
                                        > MAX_ENTRY_TITLE_WORDS)
                        })
                    })
            };
            if e.kind == "doco:SectionTitle"
                && matches!(e.evidence, "bold" | "font-size" | "numbering")
                && aligned.iter().any(entry_row)
            {
                let e = &mut elements[i];
                e.kind = "doco:Paragraph".into();
                e.level = None;
                e.evidence = "layout";
            }
        }
    }
}

fn read_table_heads(elements: &mut [Element]) {
    let heads: Vec<usize> = (0..elements.len())
        .filter(|&i| {
            let e = &elements[i];
            let Some(b) = e.bbox else {
                return false;
            };
            if e.kind != "doco:SectionTitle"
                || !matches!(e.evidence, "bold" | "font-size")
                || e.text.split_whitespace().count() > MAX_TABLE_HEAD_WORDS
            {
                return false;
            }
            let h = b.height().max(1.0);
            // Other heads set on its row: the row of a table's column heads.
            // Two or more: a form's part title beside its `Part I` label is
            // one title with its number.
            let on_a_row = |tb: &crate::geom::BBox| {
                elements
                    .iter()
                    .filter(|o| {
                        !std::ptr::eq(*o, e)
                            && o.page == e.page
                            && o.kind != "doco:Table"
                            && o.bbox.is_some_and(|ob| {
                                (ob.y0 - b.y0).abs() <= h
                                    && ob.x0 >= tb.x0 - h
                                    && ob.x1 <= tb.x1 + h
                                    && (ob.x0 >= b.x1 || ob.x1 <= b.x0)
                            })
                    })
                    .count()
                    >= 2
            };
            // Nothing between it and the table over its width: a heading
            // over its own paragraph heads the paragraph, not the table
            // after it.
            let directly_on = |tb: &crate::geom::BBox| {
                !elements.iter().any(|o| {
                    !std::ptr::eq(o, e)
                        && o.page == e.page
                        && o.kind != "doco:Table"
                        && o.bbox.is_some_and(|ob| {
                            ob.y0 >= b.y1 - 0.2 * h
                                && ob.y1 <= tb.y0 + h
                                && ob.x1.min(b.x1) > ob.x0.max(b.x0)
                        })
                })
            };
            elements.iter().any(|t| {
                t.kind == "doco:Table"
                    && t.page == e.page
                    && t.bbox.is_some_and(|tb| {
                        // A line's height is about twice its ink's.
                        let on_top = b.y1 <= tb.y0 + h && tb.y0 - b.y1 <= 2.0 * h;
                        let inside = b.x0 >= tb.x0 - h && b.x1 <= tb.x1 + h;
                        let from_edge = (b.x0 - tb.x0).abs() <= 2.0 * h;
                        let centred = ((b.x0 + b.x1) - (tb.x0 + tb.x1)).abs() * 0.5 <= 2.0 * h;
                        // Over the value columns, past the table's middle,
                        // or on the row of the table's other heads. A title
                        // set a little in from a table's edge stands alone,
                        // over its stub.
                        let over_values = b.x0 >= (tb.x0 + tb.x1) * 0.5;
                        on_top
                            && inside
                            && !from_edge
                            && !centred
                            && directly_on(&tb)
                            && (over_values || on_a_row(&tb))
                    })
            })
        })
        .collect();
    for i in heads {
        let e = &mut elements[i];
        e.kind = "doco:Paragraph".into();
        e.level = None;
        e.evidence = "layout";
    }
}

/// How far past a lead-in's end, in its line heights, the items it
/// introduces may be set in: a bullet's indent and the space after it.
const LEAD_IN_REACH: f64 = 3.0;

/// A form's label over a box left blank, read as a heading on its weight
/// or size: a word or three ending in a colon or `#` with nothing set under
/// it on its page, over its width, and nothing on its row to its right but
/// another such label: `APPROVALS:` and `AVAILABLE AT:` side by side over the
/// empty boxes in a sheet's footer, for a dealer's stamp. A heading ending
/// in a colon introduces what is set under it, a list, a table or a figure,
/// however much space it leaves first, or what runs on beside it (an
/// article's `FOURTH:` beside its text, set in past it). Run on the final
/// elements, where tables and figures stand under their labels.
fn read_blank_fields(elements: &mut [Element]) {
    let blank: Vec<usize> = (0..elements.len())
        .filter(|&i| {
            let e = &elements[i];
            let Some(b) = e.bbox else {
                return false;
            };
            let h = b.height().max(1.0);
            e.kind == "doco:SectionTitle"
                && matches!(e.evidence, "bold" | "font-size")
                && heading::form_label(e.text.trim())
                && !elements.iter().enumerate().any(|(k, o)| {
                    k != i
                        && o.page == e.page
                        && o.bbox.is_some_and(|ob| {
                            // Reaching below it over its width, or set in
                            // from it as a list's items are under their
                            // lead-in, from under it or from its row: a
                            // paragraph whose first line runs on beside the
                            // label wraps under it.
                            let reach = b.x1 + LEAD_IN_REACH * h;
                            let under =
                                ob.y1 > b.y1 + 0.2 * h && reach.min(ob.x1) > ob.x0.max(b.x0);
                            // Text set on to its right on its row is what
                            // it labels, unless that is another blank's label.
                            let beside = ob.x0 >= b.x1
                                && ob.y1.min(b.y1) - ob.y0.max(b.y0) > 0.5 * h
                                && !heading::form_label(o.text.trim());
                            under || beside
                        })
                })
        })
        .collect();
    for i in blank {
        let e = &mut elements[i];
        e.kind = "doco:Paragraph".into();
        e.level = None;
        e.evidence = "layout";
    }
}

/// Two headings that are one, set over two lines: the second goes on in
/// lower case from where the first starts, or the two are centred on one
/// axis, one line's spacing apart (`Poly-Lactic-Acid` over `FDA Compliant
/// for direct food contact`, a sheet's subtitle). Lines set from one left
/// edge and each opening with a capital are two headings, a section over
/// its first subsection; so is a division's label over its title.
fn heading_fragments_join(a: &Element, b: &Element) -> bool {
    if a.kind != "doco:SectionTitle"
        || b.kind != "doco:SectionTitle"
        || a.page != b.page
        || a.level != b.level
        || unflagged(a.evidence) != unflagged(b.evidence)
        || !matches!(unflagged(a.evidence), "font-size" | "bold")
    {
        return false;
    }
    let (Some(ab), Some(bb)) = (a.bbox, b.bbox) else {
        return false;
    };
    let height = ab.height().max(bb.height()).max(1.0);
    if bb.y0 < ab.y0 || bb.y0 - ab.y1 > 1.5 * height {
        return false;
    }
    let lower = b
        .text
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase);
    if lower {
        return (ab.x0 - bb.x0).abs() <= height;
    }
    // Centred: the two lines' midpoints within half a line's height, what a
    // centred setting's rounding and a trailing space leave between them.
    // From different left edges, by more than a line's height: two lines of
    // different lengths, not a left-aligned pair whose midpoints happen to
    // agree.
    let centre = |x: &crate::geom::BBox| (x.x0 + x.x1) / 2.0;
    unflagged(a.evidence) == "font-size"
        && (centre(&ab) - centre(&bb)).abs() <= height * 0.5
        && (ab.x0 - bb.x0).abs() > height
        && !designated_label(&a.text)
}

/// A label naming a part of a document by its designator, and nothing
/// else: `ARTICLE VII`, `EXHIBIT B`, `SECTION 4.15`, `Schedule 2.10`,
/// `Chapter 3`. It heads the title set under it, and stays a heading of its
/// own.
fn designated_label(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let (Some(word), Some(designator), None) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    let d = designator.trim_end_matches(['.', ':']);
    word.chars().all(char::is_alphabetic)
        && !d.is_empty()
        && (d.chars().all(|c| c.is_ascii_digit() || c == '.')
            || d.chars().all(|c| "IVXLCDMivxlcdm".contains(c))
            || (d.chars().count() == 1 && d.chars().all(char::is_alphabetic)))
}

fn coalesce_heading_fragments(elements: &mut Vec<Element>) {
    // Whether each element stands alone on its rows: nothing else is set
    // level with it, beside it. A subtitle centred over two lines does; the
    // cells of a table no grid took have neighbours on their rows.
    let alone: Vec<bool> = elements
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let Some(b) = e.bbox else {
                return false;
            };
            !elements.iter().enumerate().any(|(j, o)| {
                o.bbox.is_some_and(|ob| {
                    let overlap = b.y1.min(ob.y1) - b.y0.max(ob.y0);
                    let shorter = (b.y1 - b.y0).min(ob.y1 - ob.y0);
                    j != i
                        && shorter > 0.0
                        && overlap >= shorter * 0.5
                        && (ob.x1 <= b.x0 || b.x1 <= ob.x0)
                })
            })
        })
        .collect();
    let mut out: Vec<Element> = Vec::with_capacity(elements.len());
    // A heading centred over two lines is joined once: a third centred line
    // under them is a heading of its own.
    let mut joined_centred = false;
    let mut last_alone = false;
    for (e, alone) in elements.drain(..).zip(alone) {
        if let Some(previous) = out.last_mut() {
            let lower = e
                .text
                .chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(char::is_lowercase);
            if heading_fragments_join(previous, &e)
                && (lower || (!joined_centred && alone && last_alone))
            {
                joined_centred = !lower;
                previous.text.push(' ');
                previous.text.push_str(&e.text);
                // Joined, the heading is a form's text only if both parts
                // were: a line that stopped mid-phrase has found its end.
                if !e.evidence.starts_with(FORM_TEXT) {
                    previous.evidence = unflagged(previous.evidence);
                }
                if let (Some(a), Some(b)) = (previous.bbox.as_mut(), e.bbox) {
                    a.x0 = a.x0.min(b.x0);
                    a.y0 = a.y0.min(b.y0);
                    a.x1 = a.x1.max(b.x1);
                    a.y1 = a.y1.max(b.y1);
                }
                continue;
            }
        }
        joined_centred = false;
        last_alone = alone;
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
        let (marker, marker_x) = (b.marker.clone(), b.marker_x);
        let mut parts = Vec::new();
        let mut lines = b.lines;
        let tail: Vec<Line> = lines.split_off(last + 1);
        let mid: Vec<Line> = lines.split_off(first);
        if !lines.is_empty() {
            parts.push(block::build(lines));
        }
        let mut title = block::build(mid);
        title.marker = marker;
        title.marker_x = marker_x;
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
        cell.split_whitespace().count() > PROSE_CELL_WORDS
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
    let wide: Vec<&Grid> = aligned
        .iter()
        .filter(|grid| grid.cols() >= ruled_grid.cols() + 3)
        .collect();
    (coarse && !nested_in_one_row(ruled_grid, &wide))
        || aligned_cells >= ruled_grid.rows() * ruled_grid.cols() * 3
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
        cell.split_whitespace().count() > PROSE_CELL_WORDS
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
                && !nested_in_one_row(ruled_grid, &[band])
        })
}

/// The wider readings each lie within one row of a ruled grid of four rows
/// or more: grids nested in its rows (an acoustic row's nine columns of
/// figures in a property list), not tables a frame was drawn round. A long
/// cell is then no sign of a frame, and the ruled grid stands. A frame's
/// table runs down across the frame's rows, or the frame has too few rows to
/// tell.
fn nested_in_one_row(ruled: &Grid, wide: &[&Grid]) -> bool {
    ruled.rows() >= 4
        && !wide.is_empty()
        && wide.iter().all(|w| {
            ruled.ys.windows(2).any(|row| {
                w.bbox.y0 >= row[0] - table::EDGE_TOLERANCE
                    && w.bbox.y1 <= row[1] + table::EDGE_TOLERANCE
            })
        })
}

/// Most of a ruled grid's rows read as a table's rows: two cells or more
/// filled, each short, over four rows or more. Such a grid is a table
/// whatever one long cell holds, or a grid nested in one of its rows (a
/// property list whose acoustic row sets its own nine columns of figures):
/// its rules account for its cells, and no reading of the text in its place
/// does better. A frame round a page of prose, or a table set in one of its
/// cells, has few such rows; a box of three rows holding field lists says
/// too little either way.
fn reads_as_rows(grid: &Grid, glyphs: &[Glyph]) -> bool {
    let cols = grid.cols();
    if cols < 2 || grid.rows() < 4 {
        return false;
    }
    let cells = grid.cell_texts(glyphs);
    let rows = cells
        .chunks(cols)
        .filter(|row| {
            let words: Vec<usize> = row
                .iter()
                .map(|t| t.split_whitespace().count())
                .filter(|&w| w > 0)
                .collect();
            words.len() >= 2 && words.iter().all(|&w| w <= PROSE_CELL_WORDS)
        })
        .count();
    rows * 2 > grid.rows()
}

/// Words a cell holds before it is prose rather than a value: the length
/// at which a ruled grid's cell counts as coarse.
const PROSE_CELL_WORDS: usize = 25;

/// Drop the rows of a table that hold nothing: a blank row a table sets
/// between its groups of rows, ruled as its own
/// ([`table::Grid::trim_to_content_ruled`]). The row is the ruling's
/// spacing, not data: it is not emitted, and the rows under it number on
/// from the rows over it. A row a merged cell spans, its text read into the
/// cell's first row, is part of that cell and stays.
fn drop_blank_rows(e: &mut Element) {
    let Some(rows) = e.cells.as_ref() else {
        return;
    };
    let (n, cols) = (rows.len(), rows.first().map_or(0, Vec::len));
    if n < 3 || cols == 0 || rows.iter().any(|r| r.len() != cols) {
        return;
    }
    let per_cell = |v: &Option<Vec<bool>>| v.as_ref().is_none_or(|v| v.len() == n * cols);
    if !(per_cell(&e.merged_down) && per_cell(&e.merged_left) && per_cell(&e.spread_left)) {
        return;
    }
    let down = |r: usize, c: usize| e.merged_down.as_ref().is_some_and(|m| m[r * cols + c]);
    let blank: Vec<usize> = (1..n - 1)
        .filter(|&r| {
            rows[r].iter().all(|t| t.trim().is_empty())
                && (0..cols).all(|c| !down(r, c) && !down(r + 1, c))
        })
        .collect();
    if blank.is_empty() {
        return;
    }
    let keep = |r: &usize| !blank.contains(r);
    let up = |r: usize| r - blank.iter().filter(|&&b| b < r).count();
    let rows: Vec<Vec<String>> = rows
        .iter()
        .enumerate()
        .filter(|(r, _)| keep(r))
        .map(|(_, row)| row.clone())
        .collect();
    let flat = |v: &mut Option<Vec<bool>>| {
        if let Some(v) = v.as_mut() {
            *v = v
                .chunks(cols)
                .enumerate()
                .filter(|(r, _)| keep(r))
                .flat_map(|(_, row)| row.iter().copied())
                .collect();
        }
    };
    flat(&mut e.merged_down);
    flat(&mut e.merged_left);
    flat(&mut e.spread_left);
    let indexes = |v: &mut Option<Vec<usize>>| {
        if let Some(v) = v.as_mut() {
            *v = v.iter().copied().filter(keep).map(up).collect();
        }
    };
    indexes(&mut e.sub_headers);
    indexes(&mut e.restated_headers);
    if let Some(h) = e.header_rows.as_mut() {
        *h = up(*h);
    }
    e.text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    e.cells = Some(rows);
}

/// The table a grid nested in a ruled cell reads as: its own header row,
/// then a band naming the property it sits under (`Acoustical
/// Performance`), so its cells carry that as their section, then its rows.
fn nested_table(
    n: &Grid,
    page: &crate::extract::Page,
    pi: usize,
    label: Option<String>,
    scrub: &[(String, bool)],
) -> Element {
    let cols = n.cols().max(1);
    let mut rows: Vec<Vec<String>> = n
        .cell_texts(&page.glyphs)
        .chunks(cols)
        .map(|row| {
            row.iter()
                .map(|t| furniture::scrub_cell(t, scrub))
                .collect()
        })
        .collect();
    // A first row of names over rows of figures is the header; the
    // ruled-table reading decides otherwise.
    let figures = |row: &[String]| row.iter().any(|t| table::is_numeric_cell(t.trim()));
    let header_rows =
        if rows.len() > 1 && !figures(&rows[0]) && rows[1..].iter().all(|r| figures(r)) {
            1
        } else {
            n.header_rows(&rows, &page.glyphs, &page.rules, &page.fills)
        };
    let mut merged_left = vec![false; rows.len() * cols];
    let mut sub_headers = Vec::new();
    if let Some(label) = label {
        let mut band = vec![String::new(); cols];
        band[0] = label;
        rows.insert(header_rows, band);
        let at = header_rows * cols;
        merged_left.splice(at..at, (0..cols).map(|c| c > 0));
        sub_headers.push(header_rows);
    }
    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    Element {
        page: pi,
        bbox: Some(n.bbox),
        cells: Some(rows),
        header_rows: Some(header_rows),
        sub_headers: (!sub_headers.is_empty()).then_some(sub_headers),
        merged_left: merged_left.iter().any(|x| *x).then_some(merged_left),
        ..Element::new("doco:Table", text, "rust", "rules")
    }
}

/// Least glyphs a ruled cell holds before it is read for a grid of its own.
const NESTED_MIN_GLYPHS: usize = 20;

/// A grid's first row names its columns over rows of figures: a table with
/// a header of its own. A cell's figures stacked line under line under the
/// outer table's header line up into columns too, but name nothing.
fn names_its_columns(n: &Grid, glyphs: &[Glyph]) -> bool {
    let cols = n.cols().max(1);
    let cells = n.cell_texts(glyphs);
    let rows: Vec<&[String]> = cells.chunks(cols).collect();
    let figures = |row: &[String]| row.iter().any(|t| table::is_numeric_cell(t.trim()));
    let [head, body @ ..] = rows.as_slice() else {
        return false;
    };
    let names = head.iter().filter(|t| !t.trim().is_empty()).count();
    names * 2 > cols && !figures(head) && !body.is_empty() && body.iter().all(|r| figures(r))
}

/// Grids set inside single cells of a ruled table, each with the row and
/// column of the cell it fills: a property list whose acoustic row sets nine
/// columns of figures under their own header in its Performance cell. A
/// cell's text lined up into three columns or more over three rows or more,
/// taking nearly all of the cell, its first row naming the columns over rows
/// of figures, is a table of its own; wrapped prose in a cell lines up into
/// no columns.
fn nested_grids(g: &Grid, page: &crate::extract::Page) -> Vec<(Grid, usize, usize)> {
    let cols = g.cols();
    let mut by_cell: std::collections::BTreeMap<usize, Vec<Glyph>> =
        std::collections::BTreeMap::new();
    for (gl, home) in page.glyphs.iter().zip(g.glyph_cells(&page.glyphs)) {
        if let Some(k) = home.filter(|_| !gl.text.trim().is_empty()) {
            by_cell.entry(k).or_default().push(gl.clone());
        }
    }
    let mut out = Vec::new();
    for (k, inside) in by_cell {
        if inside.len() < NESTED_MIN_GLYPHS {
            continue;
        }
        for n in table::detect_aligned(&inside, &page.rules, &page.fills, page.index) {
            if n.rows() < 3 || n.cols() < 3 {
                continue;
            }
            let taken = n.takes(&inside).iter().filter(|t| **t).count();
            if taken * 10 >= inside.len() * 8 && names_its_columns(&n, &inside) {
                out.push((n, k / cols, k % cols));
            }
        }
    }
    out
}

/// A grid's first column holds nothing but list markers (`a)`, `2.`, `(iii)`,
/// any bullet [`block::is_bullet`] knows, a dash), at least two of them: an
/// enumerated list read as a table.
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
        let mut chars = t.chars();
        let bullet = chars.next().is_some_and(block::is_bullet) && chars.next().is_none();
        bullet
            || matches!(t, "-" | "–" | "*")
            || (t != body
                && (1..=4).contains(&body.chars().count())
                && body.chars().all(|c| c.is_ascii_alphanumeric()))
    };
    firsts.len() >= 2 && firsts.iter().all(|t| marker(t))
}

/// A grid read from alignment that is a bulleted list: two columns, its
/// bullets and its items ([`is_marker_list`]), with at most one first row of
/// other text over them, a heading or a running head set level with the
/// list. Its bullets line up down the page as a column does, and each item's
/// text beside them as another.
///
/// Bullets only, each marker one character. A numbered or lettered list read
/// back as prose sets each item as a numbered line, and a numbered line of
/// one item reads as a numbered section: a contract's `(b) "Board" means the
/// Board of Directors.` became a heading, and a report's list of its key
/// audit matters, `1. Measurement of shares`, repeated the numbered heads it
/// lists and cost them their numbering. Read as a table, those stay apart.
fn lists_its_rows(g: &Grid, glyphs: &[Glyph]) -> bool {
    if g.cols() != 2 {
        return false;
    }
    let cells = g.cell_texts(glyphs);
    let bulleted = |cells: &[String]| {
        is_marker_list(cells, 2)
            && cells
                .iter()
                .step_by(2)
                .all(|t| t.trim().chars().count() <= 1)
    };
    bulleted(&cells) || (g.rows() >= 3 && bulleted(&cells[2..]))
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

/// Whether a model's reading of `table` is worth asking for: its header is
/// in doubt, or its structure disagrees with itself (`suspect`, from
/// [`table::suspect_tables`]: fragments of one ruled region that disagree on
/// their columns, or rows merged into one).
///
/// A header is in doubt where it is anything but one plain row: no header
/// read at all, a header of two rows or more, a cell spanning rows or
/// columns, a banner over a group of rows, or a row restating the header
/// further down. That is where our reading of a table is weakest (which rows
/// head it, what each label spans), and where a reading's spans say what a
/// grid of cells cannot.
///
/// A table with one plain header row, drawn or inferred, gains nothing a
/// reading can add, and the reading replaces it whenever their shapes
/// differ ([`crate::arbiter::reading_wins`]), so a reading that merges two
/// columns, drops a row or takes a caption in costs the table. Measured over
/// the benchmark's 39 documents whose tables were read by a model, with two
/// readers (a live gemini-3-flash run and the older cached readings): on the
/// 30 with only plain headers, readings made 11 documents worse and 5 better
/// (summed −0.16; cached −0.36). On the 9 with a header in doubt they made 7
/// better and none worse (+0.55; cached +0.45). Drawn against inferred does
/// not separate the two: drawn grids with spanned headers were among the
/// largest gains, and inferred grids with plain headers among the largest
/// losses.
fn table_in_doubt(table: &Element, suspect: bool) -> bool {
    suspect
        || table.header_rows != Some(1)
        || table.merged_down.is_some()
        || table.merged_left.is_some()
        || table.sub_headers.is_some()
        || table.restated_headers.is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSource {
    Ruled,
    FillBand,
    Aligned,
    HorizontalBand,
    /// Read from its text by `textgrid`, with its structure stated.
    Text,
}

/// Fewest lines of prose a document's body size is read from alone.
const THIN_PROSE_LINES: usize = 8;

/// The body size of a document whose prose is too short to say it, read
/// with its tables' text: a page that is a title, a label and a folio over a
/// table. Its prose alone counts the title's two lines against one of each
/// other size, and the title, taken for the body, was no heading. Each of
/// a table's lines counts once, at its size; `None` where the prose runs
/// to [`THIN_PROSE_LINES`] or more, or no table holds text.
fn thin_prose_body(
    blocks: &[Vec<Block>],
    pages: &[crate::extract::Page],
    layouts: &[PageLayout],
) -> Option<f32> {
    let prose: usize = blocks.iter().flatten().map(|b| b.lines.len()).sum();
    if prose >= THIN_PROSE_LINES {
        return None;
    }
    let key = |size: f32| (size * 4.0).round() as i32;
    let mut lines: std::collections::BTreeMap<i32, usize> = Default::default();
    for b in blocks.iter().flatten() {
        *lines.entry(key(b.font_size)).or_default() += b.lines.len();
    }
    let mut table_lines: std::collections::BTreeSet<(usize, i64, i32)> = Default::default();
    for (pi, layout) in layouts.iter().enumerate() {
        let Some(page) = pages.get(pi) else { continue };
        for g in &layout.grids {
            for gl in page
                .glyphs
                .iter()
                .filter(|gl| gl.is_horizontal() && !gl.text.trim().is_empty())
            {
                let Some(b) = gl.bbox else { continue };
                let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                if cx > g.bbox.x0 && cx < g.bbox.x1 && cy > g.bbox.y0 && cy < g.bbox.y1 {
                    table_lines.insert((pi, gl.origin.1.round() as i64, key(gl.font_size)));
                }
            }
        }
    }
    if table_lines.is_empty() {
        return None;
    }
    for (_, _, k) in &table_lines {
        *lines.entry(*k).or_default() += 1;
    }
    // The most lines; of equal counts, the smaller size.
    lines
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(k, _)| *k as f32 / 4.0)
}

struct PageLayout {
    grids: Vec<Grid>,
    grid_sources: Vec<GridSource>,
    /// Per grid, rows whose text crosses a column boundary read from
    /// alignment rather than drawn; see [`Grid::split_by_alignment`].
    spanning_rows: Vec<Vec<usize>>,
    prose_columns: Vec<Vec<Line>>,
    /// Per prose column, the page column it is a stretch of, and whether it
    /// sets its own measure; see [`crate::region::Group`].
    prose_sources: Vec<(usize, bool)>,
    route: crate::route::Route,
    missing_tables: Vec<crate::geom::BBox>,
    demoted_tables: Vec<bool>,
    /// The lettering an OCR engine read in the page's artwork, set apart
    /// from the page's text; see [`take_lettering`].
    lettering: Vec<Lettering>,
    /// The pictures read as tables ruled by the rules in their pixels
    /// ([`picture_table`]): the page's text, as a picture read as text is,
    /// and no figure.
    picture_tables: Vec<crate::geom::BBox>,
}

/// A picture's lettering, read by OCR inside it and assembled on its own:
/// a chart's labels, a diagram's names, a logo's word.
struct Lettering {
    /// The picture.
    bbox: crate::geom::BBox,
    /// The glyphs the reading laid in it, in the page's order.
    glyphs: Vec<Glyph>,
    lines: Vec<Line>,
    /// Text set as an image ([`text_picture`]): read as the page's own text
    /// where the picture stands, not as a figure's lettering.
    text: bool,
}

/// Least share of a region's area its text covers for the region to be a
/// letter drawn as an image: the page's own text, set a character to an
/// image, which the page's text pass reads as it reads the rest.
const LETTER_IMAGE_COVER: f64 = 1.0;

/// Most a picture's height may be, in the ems of the page's text on its
/// row, for it to be a word set in that text as an image: an image set in a
/// line is sized to the line, with a margin.
const INLINE_IMAGE_HEIGHT: f64 = 2.0;

/// Widest gap, in the ems of the page's text beside it, between a word set
/// as an image and that text: a space or two, as between words.
const INLINE_IMAGE_GAP_EM: f64 = 2.0;

/// Is a picture holding one line of text a word set as an image in a line of
/// the page's own text? Some glyph of the page's stands on its row, no
/// more than [`INLINE_IMAGE_GAP_EM`] from it, the picture no taller than
/// [`INLINE_IMAGE_HEIGHT`] of it: a wordmark among a chart's labels.
fn inline_image(b: &crate::geom::BBox, glyphs: &[Glyph]) -> bool {
    let h = b.height();
    glyphs.iter().any(|g| {
        let Some(gb) = g
            .bbox
            .filter(|_| g.ocr.is_none() && g.rotation_deg.abs() < 1.0 && !g.text.trim().is_empty())
        else {
            return false;
        };
        let em = f64::from(g.font_size);
        let overlap = gb.y1.min(b.y1) - gb.y0.max(b.y0);
        let gap = (gb.x0 - b.x1).max(b.x0 - gb.x1).max(0.0);
        em > 0.0
            && overlap >= 0.5 * gb.height().min(h)
            && h <= INLINE_IMAGE_HEIGHT * em
            && gap <= INLINE_IMAGE_GAP_EM * em
    })
}

/// Least share of a picture's width a line of its text spans to be a line
/// set across it, as prose, a wordmark or a table's row is: a chart's
/// labels and a diagram's names stand in a part of it.
const TEXT_PICTURE_LINE: f64 = 0.6;

/// Least share of a picture's area its text covers for the picture to be
/// text set as an image: a table, a label's panel, a page of prose. A
/// chart's or a diagram's labels cover a tenth of it or less.
const TEXT_PICTURE_COVER: f64 = 0.2;

/// Is a picture whose text reads `lines` text set as an image, rather than
/// artwork lettered here and there? Most of its lines run across it, or its
/// text covers a good share of it.
fn text_picture(bbox: &crate::geom::BBox, lines: &[Line]) -> bool {
    let lines: Vec<&Line> = lines.iter().filter(|l| !l.text.trim().is_empty()).collect();
    if lines.is_empty() {
        return true;
    }
    let (w, h) = (bbox.width().max(1.0), bbox.height().max(1.0));
    let across = lines
        .iter()
        .filter(|l| l.bbox.width() >= w * TEXT_PICTURE_LINE)
        .count();
    let cover: f64 = lines
        .iter()
        .map(|l| l.bbox.width() * l.bbox.height())
        .sum::<f64>()
        / (w * h);
    across * 2 >= lines.len() || cover >= TEXT_PICTURE_COVER
}

/// Take the lettering an OCR engine read in the page's artwork out of its
/// glyphs, each picture's assembled on its own. Read with the page's text,
/// a chart's labels set at the chart's pixels made and broke the page's
/// gutters, were read line by line into the prose beside them, and were
/// gridded as tables. A picture that is text set as an image
/// ([`text_picture`]) is read apart too, as the page's own text where it
/// stands. A page read whole, and a letter drawn as an image of its own
/// ([`LETTER_IMAGE_COVER`]), keep their text as the page's.
/// Returns too the pictures read as tables ([`picture_table`]), whose text
/// stays the page's and whose rules join the page's.
fn take_lettering(p: &mut Page) -> (Vec<Lettering>, Vec<crate::geom::BBox>) {
    let lettered = |g: &Glyph| g.ocr == Some(crate::glyph::OcrSource::Region);
    if !p.glyphs.iter().any(|g| lettered(g) && g.bbox.is_some()) {
        return (Vec::new(), Vec::new());
    }
    // What the readings were taken for: the routed regions, each one
    // picture whatever images draw it (a chromatogram set a letter to an
    // image); or, without them, the page's images.
    let regions: Vec<crate::geom::BBox> = p
        .ocr
        .as_ref()
        .map(|o| {
            o.region_boxes
                .iter()
                .copied()
                .filter(|b| b.width() > 0.0 && b.height() > 0.0)
                .collect()
        })
        .unwrap_or_default();
    let bases: Vec<crate::geom::BBox> = if regions.is_empty() {
        p.images.iter().map(|im| im.bbox).collect()
    } else {
        regions
    };
    // Each glyph's base: the one holding its middle, or else the nearest. A
    // reading keeps a word whose middle lies in a region, and the word's
    // last letters may run past the region's edge. The space a reading lays
    // between its words has no box, and goes where its pen stands: without
    // it, a chart's figures run together (`98.698.5`).
    let mut held: Vec<Vec<usize>> = vec![Vec::new(); bases.len()];
    for (i, g) in p.glyphs.iter().enumerate() {
        if !lettered(g) {
            continue;
        }
        let (x, y) = match g.bbox {
            Some(b) => ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5),
            None => g.origin,
        };
        let away = |r: &crate::geom::BBox| {
            let dx = (r.x0 - x).max(x - r.x1).max(0.0);
            let dy = (r.y0 - y).max(y - r.y1).max(0.0);
            dx.hypot(dy)
        };
        let home = bases.iter().position(|r| away(r) == 0.0).or_else(|| {
            (0..bases.len()).min_by(|&a, &b| away(&bases[a]).total_cmp(&away(&bases[b])))
        });
        if let Some(k) = home {
            held[k].push(i);
        }
    }
    let lines_of = |idx: &[usize], glyphs: &[Glyph]| {
        let gs: Vec<Glyph> = idx.iter().map(|&i| glyphs[i].clone()).collect();
        crate::line::assemble(&gs)
    };
    // A base whose text is the page's own, set as an image, stays the
    // page's text: a letter drawn as an image of its own, its text covering
    // it whole, or a word set as an image in a line of the page's text
    // ([`inline_image`]). The rest that touch or overlap are one picture: a
    // mosaic of tiles, or a line of text cut across images.
    let mut rest: Vec<(crate::geom::BBox, Vec<usize>)> = Vec::new();
    for (b, idx) in bases.into_iter().zip(held) {
        if !idx.is_empty() {
            let lines = lines_of(&idx, &p.glyphs);
            let (w, h) = (b.width().max(1.0), b.height().max(1.0));
            let cover: f64 = lines
                .iter()
                .map(|l| l.bbox.width() * l.bbox.height())
                .sum::<f64>()
                / (w * h);
            if cover >= LETTER_IMAGE_COVER || (lines.len() == 1 && inline_image(&b, &p.glyphs)) {
                continue;
            }
        }
        rest.push((b, idx));
    }
    let mut pictures: Vec<(crate::geom::BBox, Vec<usize>)> = Vec::new();
    for (mut b, mut idx) in rest {
        loop {
            let before = pictures.len();
            pictures.retain(|(o, oi)| {
                let touches = o.x0 <= b.x1 + 1.0
                    && b.x0 <= o.x1 + 1.0
                    && o.y0 <= b.y1 + 1.0
                    && b.y0 <= o.y1 + 1.0;
                if touches {
                    b = b.union(o);
                    idx.extend_from_slice(oi);
                }
                !touches
            });
            if pictures.len() == before {
                break;
            }
        }
        pictures.push((b, idx));
    }
    let mut taken = vec![false; p.glyphs.len()];
    let mut out = Vec::new();
    // The rules of the pictures read as tables, laid on the page once all
    // pictures are judged.
    let mut ruling: Vec<crate::rule::Rule> = Vec::new();
    let mut tables: Vec<crate::geom::BBox> = Vec::new();
    for (bbox, mut idx) in pictures {
        if idx.is_empty() {
            continue;
        }
        idx.sort_unstable();
        let glyphs: Vec<Glyph> = idx.iter().map(|&i| p.glyphs[i].clone()).collect();
        let lines = crate::line::assemble(&glyphs);
        let text = text_picture(&bbox, &lines);
        // A table set as an image: its text stays the page's, and the rules
        // drawn in its pixels join the page's, so the page's table passes
        // read it as a table drawn with rules. Only here, and only for a
        // picture whose own rules grid its own text ([`picture_table`]),
        // do a picture's rules become the page's. Any picture is asked, a
        // text picture or not: a table's cells are short runs, neither
        // lines across it nor much of its area, and the test is the grid.
        let rules = picture_rules(p, &bbox);
        if picture_table(&glyphs, &rules, p.index) {
            ruling.extend(rules);
            tables.push(bbox);
            continue;
        }
        for &i in &idx {
            taken[i] = true;
        }
        out.push(Lettering {
            bbox,
            glyphs,
            lines,
            text,
        });
    }
    if !out.is_empty() {
        let mut k = taken.iter();
        p.glyphs.retain(|_| !*k.next().unwrap_or(&false));
    }
    p.rules.extend(ruling);
    (out, tables)
}

/// Least share of a picture's words a grid of its rules holds for the
/// picture to be a table. A table set as an image is most of the image: a
/// title or a source note may stand over or under its ruling, but its words
/// are in its cells. A diagram's boxes each hold a label or two, and its
/// arrows' labels, legend and title stand outside them.
const PICTURE_TABLE_WORDS: f64 = 0.8;

/// Least share of a grid's cells, a merged cell counted once, that hold
/// text for the grid to be a table's. A table leaves some cells empty (one
/// figure a row across three columns leaves two of them empty in each row);
/// a lattice of a diagram's boxes, or of a form's or a screen's frames, is
/// mostly empty space between its labels.
const PICTURE_TABLE_FILLED: f64 = 0.5;

/// Least share of the extent of a picture's text, across and down, that the
/// grid spans: a table's ruling runs across all of it. A box drawn around
/// one part of a picture, a panel of a screenshot or a key, does not.
const PICTURE_TABLE_SPAN: f64 = 0.75;

/// Most share of a grid's filled cells, a merged cell counted once, whose
/// text breaks apart along a line ([`SCATTER_EMS`]) for the grid to be a
/// table's. A table's cell holds one run of text a line, a figure or a
/// label, however wide the cell; a chart's plot, banded by its gridlines,
/// holds its values set apart across each band (`58,1 … 58,3 … 57,4`),
/// and the bands are cells only of the gridlines' making.
const PICTURE_TABLE_SCATTERED: f64 = 0.25;

/// Widest gap between two words on one line of a cell, in the size of
/// their text, that is still the space between words: a tab's width, more
/// than any justified line opens, less than the distance between two
/// values a chart sets apart.
const SCATTER_EMS: f64 = 2.5;

/// Least share of the edges between a grid's cells, inside it, that its
/// rules draw for the grid to be a table's. A table drawn with rules draws
/// the edges between its cells, all but the few a merged cell spans; the
/// boxes of a diagram, the buttons of a screen and the panels of a drawing
/// line up, but leave the edges between them undrawn, and a chart's
/// gridlines run one way only.
const PICTURE_TABLE_DRAWN: f64 = 0.75;

/// Least share of a grid's rows that hold two filled cells or more, side
/// by side, for the grid to be a table's. A table sets things beside each
/// other in its rows, a label and its figures; a framed list or a screen's
/// panel holds one block of text a row, whatever its frame draws beside it.
const PICTURE_TABLE_SIDE_BY_SIDE: f64 = 0.5;

/// The rules the routed regions' readings found drawn in this picture
/// ([`crate::extract::OcrRead::rules`]), cut to it.
fn picture_rules(p: &Page, bbox: &crate::geom::BBox) -> Vec<crate::rule::Rule> {
    let Some(read) = p.ocr.as_ref() else {
        return Vec::new();
    };
    read.rules
        .iter()
        .filter_map(|r| {
            let (cx, cy) = ((r.bbox.x0 + r.bbox.x1) * 0.5, (r.bbox.y0 + r.bbox.y1) * 0.5);
            if cx < bbox.x0 || cx > bbox.x1 || cy < bbox.y0 || cy > bbox.y1 {
                return None;
            }
            let cut = crate::geom::BBox {
                x0: r.bbox.x0.max(bbox.x0),
                y0: r.bbox.y0.max(bbox.y0),
                x1: r.bbox.x1.min(bbox.x1),
                y1: r.bbox.y1.min(bbox.y1),
            };
            (cut.x1 > cut.x0 && cut.y1 > cut.y0).then_some(crate::rule::Rule { bbox: cut, ..*r })
        })
        .collect()
}

/// Is a picture's text a table ruled by the rules drawn in its pixels? Read
/// as a drawn table is ([`table::detect_framed`]), on the picture's own
/// glyphs and rules alone: a grid of two rows and two columns at least,
/// header rows counted, that holds the picture's words
/// ([`PICTURE_TABLE_WORDS`]), its cells mostly filled
/// ([`PICTURE_TABLE_FILLED`]), each a run of text, not values set apart
/// ([`PICTURE_TABLE_SCATTERED`]), most rows setting cells side by side
/// ([`PICTURE_TABLE_SIDE_BY_SIDE`]), the edges between its cells drawn
/// ([`PICTURE_TABLE_DRAWN`]), and its ruling across the picture's text
/// ([`PICTURE_TABLE_SPAN`]).
fn picture_table(glyphs: &[Glyph], rules: &[crate::rule::Rule], page: usize) -> bool {
    use crate::rule::Orientation;
    let axis = |o: Orientation| rules.iter().filter(|r| r.orientation == o).count();
    if axis(Orientation::Horizontal) < 2 || axis(Orientation::Vertical) < 2 {
        return false;
    }
    let words: Vec<crate::geom::BBox> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .filter_map(|g| g.bbox)
        .collect();
    let Some(extent) = words.iter().copied().reduce(|a, b| a.union(&b)) else {
        return false;
    };
    let mut sizes: Vec<f64> = glyphs
        .iter()
        .filter(|g| !g.text.trim().is_empty())
        .map(|g| f64::from(g.font_size))
        .collect();
    sizes.sort_by(f64::total_cmp);
    let size = sizes[sizes.len() / 2].max(1.0);
    let mut grids = table::detect_framed(rules, &[], glyphs, page, &[]);
    for g in grids.iter_mut() {
        g.trim_to_content(glyphs);
    }
    grids.retain(|g| g.rows() >= 2 && g.cols() >= 2);
    let judged = grids.iter().map(|g| {
        let inside = |b: &crate::geom::BBox| {
            let (x, y) = ((b.x0 + b.x1) * 0.5, (b.y0 + b.y1) * 0.5);
            x >= g.bbox.x0 && x <= g.bbox.x1 && y >= g.bbox.y0 && y <= g.bbox.y1
        };
        let held = words.iter().filter(|b| inside(b)).count() as f64 / words.len() as f64;
        let cells = g.cell_texts(glyphs);
        let m = g.merges(rules, &[]);
        let cols = g.cols();
        let owner = |mut i: usize| {
            while m.continues_left[i] || m.continues_above[i] {
                i -= if m.continues_left[i] { 1 } else { cols };
            }
            i
        };
        let mut full = vec![false; cells.len()];
        for (i, c) in cells.iter().enumerate() {
            if !c.trim().is_empty() {
                full[owner(i)] = true;
            }
        }
        let slots = (0..cells.len()).filter(|&i| owner(i) == i).count().max(1);
        let filled = (0..cells.len())
            .filter(|&i| owner(i) == i && full[i])
            .count() as f64
            / slots as f64;
        // Each filled cell's glyphs, line by line, left to right: a line
        // broken by a gap wider than SCATTER_EMS is text set apart.
        let mut held_by: Vec<Vec<&Glyph>> = vec![Vec::new(); cells.len()];
        for (gl, home) in glyphs.iter().zip(g.glyph_cells(glyphs)) {
            if let (Some(i), Some(_)) = (home, gl.bbox) {
                if !gl.text.trim().is_empty() {
                    held_by[owner(i)].push(gl);
                }
            }
        }
        let broken = |cell: &Vec<&Glyph>| {
            let mut lines: Vec<Vec<&Glyph>> = Vec::new();
            let mut sorted = cell.clone();
            sorted.sort_by(|a, b| a.origin.1.total_cmp(&b.origin.1));
            for gl in sorted {
                let size = f64::from(gl.font_size).max(1.0);
                match lines.last_mut() {
                    Some(l) if (l[0].origin.1 - gl.origin.1).abs() <= 0.5 * size => l.push(gl),
                    _ => lines.push(vec![gl]),
                }
            }
            lines.iter_mut().any(|l| {
                l.sort_by(|a, b| a.bbox.unwrap().x0.total_cmp(&b.bbox.unwrap().x0));
                l.windows(2).any(|w| {
                    let (a, b) = (w[0].bbox.unwrap(), w[1].bbox.unwrap());
                    let size = f64::from(w[0].font_size.max(w[1].font_size)).max(1.0);
                    b.x0 - a.x1 > SCATTER_EMS * size
                })
            })
        };
        let full_cells = (0..cells.len())
            .filter(|&i| owner(i) == i && full[i])
            .count()
            .max(1);
        let scattered = (0..cells.len())
            .filter(|&i| owner(i) == i && full[i] && broken(&held_by[i]))
            .count() as f64
            / full_cells as f64;
        // Rows with two filled cells or more.
        let rows = g.rows();
        let side_by_side = (0..rows)
            .filter(|&r| {
                (0..cols)
                    .filter(|&c| owner(r * cols + c) == r * cols + c && full[r * cols + c])
                    .count()
                    >= 2
            })
            .count() as f64
            / rows.max(1) as f64;
        // The inner edges between cells a rule draws along half their
        // length or more, a rule standing within a quarter of the text's
        // size of the edge.
        let near = 0.25 * size;
        let drawn_along = |o: Orientation, at: f64, a: f64, b: f64| {
            let mut spans: Vec<(f64, f64)> = rules
                .iter()
                .filter(|r| r.orientation == o && (r.axis_pos() - at).abs() <= near)
                .map(|r| match o {
                    Orientation::Horizontal => (r.bbox.x0.max(a), r.bbox.x1.min(b)),
                    Orientation::Vertical => (r.bbox.y0.max(a), r.bbox.y1.min(b)),
                })
                .filter(|(x0, x1)| x1 > x0)
                .collect();
            spans.sort_by(|x, y| x.0.total_cmp(&y.0));
            let (mut got, mut reach) = (0.0, a);
            for (x0, x1) in spans {
                let x0 = x0.max(reach);
                if x1 > x0 {
                    got += x1 - x0;
                    reach = x1;
                }
            }
            got >= 0.5 * (b - a)
        };
        let mut edges = 0usize;
        let mut drawn = 0usize;
        for k in 1..g.xs.len().saturating_sub(1) {
            for r in 0..rows {
                edges += 1;
                drawn += usize::from(drawn_along(
                    Orientation::Vertical,
                    g.xs[k],
                    g.ys[r],
                    g.ys[r + 1],
                ));
            }
        }
        for k in 1..g.ys.len().saturating_sub(1) {
            for c in 0..cols {
                edges += 1;
                drawn += usize::from(drawn_along(
                    Orientation::Horizontal,
                    g.ys[k],
                    g.xs[c],
                    g.xs[c + 1],
                ));
            }
        }
        let drawn = drawn as f64 / edges.max(1) as f64;
        let across =
            (g.bbox.x1.min(extent.x1) - g.bbox.x0.max(extent.x0)) / extent.width().max(1e-9);
        let down =
            (g.bbox.y1.min(extent.y1) - g.bbox.y0.max(extent.y0)) / extent.height().max(1e-9);
        held >= PICTURE_TABLE_WORDS
            && filled >= PICTURE_TABLE_FILLED
            && scattered <= PICTURE_TABLE_SCATTERED
            && drawn >= PICTURE_TABLE_DRAWN
            && side_by_side >= PICTURE_TABLE_SIDE_BY_SIDE
            && across >= PICTURE_TABLE_SPAN
            && down >= PICTURE_TABLE_SPAN
    });
    judged.fold(false, |a, b| a || b)
}

/// The blocks a picture's text reads into, assembled on their own: a
/// figure's lettering, or the paragraphs of text set as an image. `lines`
/// are the picture's lines the furniture pass left. The one place a
/// picture's text becomes blocks, where a reader of its own (a table's, for
/// text set as a grid, `pic.text`) can take the picture's glyphs first.
fn picture_blocks(pic: &Lettering, lines: &[Line], leading: f64) -> Vec<Block> {
    debug_assert!(lines.len() <= pic.lines.len() && pic.glyphs.len() >= lines.len());
    block::assemble(lines, leading)
}

/// How many lines a page's prose columns hold: where its lettering's lines
/// start in the furniture pass's numbering.
fn kept_columns_len(cols: &[Vec<Line>]) -> usize {
    cols.iter().map(Vec::len).sum()
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
/// Is block `i` a caption's title, set on one line with the caption's label
/// as a block of its own (`FIGURE 17`, then `Renewable energy purchasing…`)?
fn labelled_caption(blocks: &[Block], i: usize) -> bool {
    let b = &blocks[i];
    blocks.iter().enumerate().any(|(j, l)| {
        let text = l.text();
        j != i
            && text.split_whitespace().count() <= 2
            && heading::is_caption(&text)
            && l.bbox.y1 > b.bbox.y0
            && l.bbox.y0 < b.bbox.y1
            && l.bbox.x1 <= b.bbox.x0 + 1.0
    })
}

/// Each page's figures, and the figure each block belongs to, if any: the
/// labels its figure took, and whatever lies mostly inside the drawing
/// itself.
fn find_figures(
    pages: &[Page],
    blocks_per_page: &[Vec<Block>],
    layouts: &[PageLayout],
) -> Vec<(Vec<figure::Figure>, Vec<Option<usize>>)> {
    let mut staged = Vec::with_capacity(blocks_per_page.len());
    for (pi, blocks) in blocks_per_page.iter().enumerate() {
        // Chart and diagram regions. Text inside one is real and its *order*
        // is not: a donut's labels and percentages interleave by position on
        // the page, so emitting them as consecutive paragraphs invites a
        // reader to pair them, and half of those pairings are wrong. They are
        // gathered into one figure instead, which says what is known (these
        // fragments belong together, here) without asserting what is not.
        let page = &pages[pi];
        let mut figures = figure::detect(
            &page.fills,
            &page.rules,
            &page.glyphs,
            pi,
            (page.width, page.height),
        );
        // A picture whose lettering was read apart is a figure, its lettering
        // the figure's text, unless a drawing found over it already is.
        if let Some(layout) = layouts.get(pi) {
            for pic in layout.lettering.iter().filter(|l| !l.text) {
                let covered = figures.iter().any(|f| {
                    figure::share_inside(&f.drawing, &pic.bbox) > 0.0
                        || figure::share_inside(&pic.bbox, &f.drawing) > 0.0
                });
                if !covered {
                    figures.push(figure::Figure {
                        bbox: pic.bbox,
                        drawing: pic.bbox,
                        page: pi,
                        shapes: 0,
                        frame: None,
                        axes: false,
                    });
                }
            }
        }
        // The drawing does not contain its own labels; take them, and grow
        // each region over them. A list item is not a label: a bulleted
        // list set under a drawing is the page's. A box drawn beside a line
        // is not a bullet, though: by a chart it is a legend's swatch, and
        // the line its label.
        let block_texts: Vec<String> = blocks.iter().map(|b| b.text()).collect();
        let candidates: Vec<usize> = (0..blocks.len())
            .filter(|&i| {
                blocks[i]
                    .marker
                    .as_deref()
                    .is_none_or(|m| m == block::CHECKBOX_MARKER)
                    && block::strip_leading_bullet(&block_texts[i]).is_none()
            })
            .collect();
        let block_boxes: Vec<(crate::geom::BBox, f32, &str)> = candidates
            .iter()
            .map(|&i| (blocks[i].bbox, blocks[i].font_size, block_texts[i].as_str()))
            .collect();
        // A picture whose text was read as the page's own is no figure, nor
        // one read as a table.
        let text_pictures: Vec<crate::geom::BBox> = layouts
            .get(pi)
            .map(|l| {
                l.lettering
                    .iter()
                    .filter(|t| t.text)
                    .map(|t| t.bbox)
                    .chain(l.picture_tables.iter().copied())
                    .collect()
            })
            .unwrap_or_default();
        let images: Vec<crate::geom::BBox> = page
            .images
            .iter()
            .map(|im| im.bbox)
            .filter(|im| {
                !text_pictures
                    .iter()
                    .any(|t| figure::share_inside(im, t) > 0.5)
            })
            .collect();
        let labelled = figure::pictures(
            &images,
            &block_boxes,
            pi,
            (page.width, page.height),
            prose_size(blocks),
            &figures,
        );
        figures.extend(labelled);
        let plotted = figure::graticules(
            &page.rules,
            &page.glyphs,
            pi,
            (page.width, page.height),
            &figures,
        );
        figures.extend(plotted);
        let charted = figure::axis_plots(
            &page.rules,
            &page.ticks,
            &page.traces,
            &page.glyphs,
            pi,
            &figures,
        );
        figures.extend(charted);
        let barred = figure::bar_plots(
            &page.fills,
            &page.rules,
            &page.ticks,
            &page.glyphs,
            pi,
            &figures,
        );
        figures.extend(barred);
        let tables: Vec<crate::geom::BBox> = layouts
            .get(pi)
            .map(|l| l.grids.iter().map(|g| g.bbox).collect())
            .unwrap_or_default();
        let diagrams = figure::box_diagrams(
            &page.fills,
            &page.rules,
            &page.glyphs,
            pi,
            (page.width, page.height),
            &tables,
            &figures,
        );
        figures.extend(diagrams);
        // A dimension drawing whose lines made a ruled grid of its callouts:
        // the drawing its loose callouts and view names are labels of.
        if let Some(layout) = layouts.get(pi) {
            let dimensioned: Vec<figure::Figure> = layout
                .grids
                .iter()
                .zip(&layout.grid_sources)
                .filter(|(g, source)| {
                    **source == GridSource::Ruled
                        && !figures
                            .iter()
                            .any(|f| figure::share_inside(&g.bbox, &f.drawing) > 0.0)
                        && dimension_drawing(g, page, &g.cell_texts(&page.glyphs))
                })
                .map(|(g, _)| figure::Figure {
                    bbox: g.bbox,
                    drawing: g.bbox,
                    page: pi,
                    // Its strokes: a drawing's, whose labels lie round it,
                    // not a picture's, whose labels are set on it.
                    shapes: page
                        .rules
                        .iter()
                        .filter(|r| figure::share_inside(&r.bbox, &g.bbox) > 0.0)
                        .count()
                        .max(1),
                    frame: None,
                    axes: false,
                })
                .collect();
            figures.extend(dimensioned);
        }
        let mut taken = figure::attach_beside(
            &mut figures,
            &block_boxes,
            &images,
            (page.width, page.height),
            prose_size(blocks),
            &figure::marks(&page.fills),
        );
        let items: Vec<(crate::geom::BBox, f32)> = (0..blocks.len())
            .filter(|i| !candidates.contains(i))
            .map(|i| (blocks[i].bbox, blocks[i].font_size))
            .collect();
        figure::release_lead_ins(&mut taken, &block_boxes, &items);
        let item_boxes: Vec<crate::geom::BBox> = items.iter().map(|(b, _)| *b).collect();
        figure::release_behind_items(&mut taken, &figures, &block_boxes, &item_boxes);
        figure::reassign_between(&mut taken, &figures, &block_boxes, prose_size(blocks));
        staged.push((figures, taken, candidates));
    }
    assign_captions(blocks_per_page, &mut staged);
    let mut page_figures = Vec::with_capacity(staged.len());
    for (pi, (figures, taken, candidates)) in staged.into_iter().enumerate() {
        let blocks = &blocks_per_page[pi];
        let block_texts: Vec<String> = blocks.iter().map(|b| b.text()).collect();
        // Which figure each block belongs to, if any: the labels its figure
        // took, and whatever lies mostly inside the drawing itself. The
        // rectangle around a drawing and its labels is not membership: a
        // paragraph or a heading inside it that was never taken as a label
        // is the page's.
        //
        // Region and heading evidence arbitrate rather than one always
        // winning. Outline/title/numbering evidence is authoritative enough
        // to survive an over-grown figure region. Relative typography is not:
        // chart legends and axis labels are commonly bold or larger than body
        // text, and treating every one as a section is the dominant
        // over-promotion failure on the benchmark.
        let mut member: Vec<Option<usize>> = vec![None; blocks.len()];
        for (fi, t) in taken.iter().enumerate() {
            for &k in t {
                member[candidates[k]].get_or_insert(fi);
            }
        }
        for (i, b) in blocks.iter().enumerate() {
            if member[i].is_none() {
                member[i] = figures
                    .iter()
                    .position(|f| figure::share_inside(&b.bbox, &f.drawing) >= FIGURE_CONTAINMENT);
            }
        }
        // A paragraph of the page's prose is no figure's, wherever the
        // figure's region reaches: set at the body's size, over two lines or
        // more, more words than a label says, and no caption or note. Shapes
        // grouped across a column, or a panel of products, enclose the text
        // beside or between them (a column's `Discussion` under the chart
        // beside it). A box diagram's steps are its own, set in its boxes.
        // The heading set over such a paragraph goes with it.
        let prose = prose_size(blocks) as f64;
        let mut released = vec![false; blocks.len()];
        for (i, b) in blocks.iter().enumerate() {
            let Some(fi) = member[i] else { continue };
            if figures[fi].frame == Some(figures[fi].drawing) {
                continue;
            }
            let text = &block_texts[i];
            let size = (b.font_size as f64).max(1.0);
            if text.split_whitespace().count() > FIGURE_PROSE_WORDS
                && (size - prose).abs() <= prose * FIGURE_PROSE_SIZE
                && b.bbox.height() >= size * 1.8
                && !heading::is_caption(text)
                && !figure::note_label(text)
            {
                member[i] = None;
                released[i] = true;
            }
        }
        for (i, b) in blocks.iter().enumerate() {
            if member[i].is_none() {
                continue;
            }
            let text = &block_texts[i];
            let size = (b.font_size as f64).max(1.0);
            let heads = blocks.iter().enumerate().any(|(j, p)| {
                released[j]
                    && p.bbox.y0 >= b.bbox.y1 - 1.0
                    && p.bbox.y0 - b.bbox.y1 <= size * 1.5
                    && (p.bbox.x0 - b.bbox.x0).abs() <= size
            });
            if heads
                && text.split_whitespace().count() <= FIGURE_PROSE_WORDS
                && !heading::is_caption(text)
                && !figure::note_label(text)
            {
                member[i] = None;
            }
        }
        page_figures.push((figures, member));
    }
    page_figures
}

/// Give each figure caption, `Figure 3.` or `Fig. 2`, to the figure it
/// names: the nearest figure on the side the document sets its captions,
/// above or below, as the captions that stand by one figure alone show it.
/// A caption set over the figure it names was taken by the figure above
/// it, standing under that one; a caption under its chart, wider than the
/// chart, was taken by none. Where the document shows no side, the nearest
/// figure takes it. A caption with no figure across from it within reach
/// stays where it is: its chart was not found. The lines a caption runs on
/// into go with it.
fn assign_captions(
    blocks_per_page: &[Vec<Block>],
    staged: &mut [(Vec<figure::Figure>, Vec<Vec<usize>>, Vec<usize>)],
) {
    use figure::CaptionSide;
    // Each page's captions, by index into its candidates, with where they
    // stand to each figure.
    let mut found: Vec<Vec<(usize, Vec<(usize, CaptionSide, f64)>)>> = Vec::new();
    let (mut above, mut below) = (0usize, 0usize);
    for (pi, (figures, taken, candidates)) in staged.iter().enumerate() {
        let blocks = &blocks_per_page[pi];
        let mut page = Vec::new();
        for (k, &bi) in candidates.iter().enumerate() {
            let b = &blocks[bi];
            if !figure::figure_caption(&b.text()) {
                continue;
            }
            // A figure's reach: its drawing and the labels it took, other
            // than captions.
            let held: Vec<usize> = taken.iter().flatten().copied().collect();
            let rel: Vec<(usize, CaptionSide, f64)> = figures
                .iter()
                .enumerate()
                .filter_map(|(fi, f)| {
                    // Measured from the drawing: the labels a figure took
                    // can reach past its caption (a heading under it).
                    let extent = f.drawing;
                    let (side, gap) = figure::caption_side(&b.bbox, b.font_size, &extent)?;
                    // Nothing of the page's prose between them: a paragraph
                    // set between a figure and a caption parts them.
                    let (top, bottom) = match side {
                        CaptionSide::Below => (extent.y1, b.bbox.y0),
                        CaptionSide::Above => (b.bbox.y1, extent.y0),
                    };
                    let parted = candidates.iter().enumerate().any(|(j, &bj)| {
                        let o = &blocks[bj];
                        j != k
                            && !held.contains(&j)
                            && o.bbox.y0 >= top - 1.0
                            && o.bbox.y1 <= bottom + 1.0
                            && o.bbox.x1.min(b.bbox.x1) > o.bbox.x0.max(b.bbox.x0)
                            && {
                                let text = o.text();
                                let words: Vec<&str> = text.split_whitespace().collect();
                                let lower = words
                                    .iter()
                                    .filter(|w| w.chars().next().is_some_and(char::is_lowercase))
                                    .count();
                                words.len() >= CAPTION_PARTED_WORDS && lower * 2 >= words.len()
                            }
                    });
                    (!parted).then_some((fi, side, gap))
                })
                .collect();
            if let [(_, side, _)] = rel.as_slice() {
                match side {
                    CaptionSide::Above => above += 1,
                    CaptionSide::Below => below += 1,
                }
            }
            page.push((k, rel));
        }
        found.push(page);
    }
    let votes = above + below;
    let side = if votes >= CAPTION_VOTES && above * 4 >= votes * 3 {
        Some(CaptionSide::Above)
    } else if votes >= CAPTION_VOTES && below * 4 >= votes * 3 {
        Some(CaptionSide::Below)
    } else {
        None
    };
    for (pi, page) in found.into_iter().enumerate() {
        let blocks = &blocks_per_page[pi];
        let (_, taken, candidates) = &mut staged[pi];
        for (k, rel) in page {
            let pick = rel
                .iter()
                .filter(|(_, s, _)| side.is_none_or(|want| *s == want))
                .min_by(|a, b| a.2.total_cmp(&b.2))
                .map(|&(fi, _, _)| fi);
            // The caption and the lines it runs on into.
            let mut unit = vec![k];
            loop {
                let last = &blocks[candidates[*unit.last().unwrap()]];
                let next = (0..candidates.len()).find(|&j| {
                    !unit.contains(&j) && {
                        let b = &blocks[candidates[j]];
                        !figure::figure_caption(&b.text())
                            && figure::caption_runs_on(
                                &last.bbox,
                                last.font_size,
                                &b.bbox,
                                b.font_size,
                            )
                            && b.bbox.y0 >= last.bbox.y1 - 1.0
                    }
                });
                match next {
                    Some(j) => unit.push(j),
                    None => break,
                }
            }
            match pick {
                Some(fi) => {
                    for (gi, t) in taken.iter_mut().enumerate() {
                        if gi != fi {
                            t.retain(|x| !unit.contains(x));
                        }
                    }
                    for &u in &unit {
                        if !taken[fi].contains(&u) {
                            taken[fi].push(u);
                        }
                    }
                    taken[fi].sort_unstable();
                }
                // No figure across from it within reach: where it stands,
                // a chart's title whose chart was not found, it stays.
                None => {}
            }
        }
    }
}

/// Fewest captions standing by one figure alone that settle the side a
/// document sets its captions on; three in four of them must agree.
const CAPTION_VOTES: usize = 2;

/// Fewest words in a block of the page's prose, half of them opening in
/// lower case, that parts a caption from a figure: an axis's values and its
/// name (`XS S M L XL Corpus Size`) are no sentence.
const CAPTION_PARTED_WORDS: usize = 6;

/// The blocks a figure holds, by page: they do not count toward the body
/// size. The body is the text set as running prose. A wiring diagram's
/// one-line labels, or a catalogue's drawings, outnumber a page's lines of
/// prose, and measured over them the body came out at the labels' 4pt:
/// every caption over it was "larger than body", and a heading.
///
/// A picture's labels (see [`figure::pictures`]) still count: a picture
/// holds a few, and set aside they tipped the body of a slide deck, whose
/// sizes stand close in count, to another size.
fn drawn(page_figures: &[(Vec<figure::Figure>, Vec<Option<usize>>)]) -> Vec<Vec<bool>> {
    page_figures
        .iter()
        .map(|(figures, member)| {
            member
                .iter()
                .map(|m| m.is_some_and(|fi| figures[fi].shapes > 0 && !figures[fi].axes))
                .collect()
        })
        .collect()
}

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
pub fn routing_is_consulted(opts: &AnalyzeOptions) -> bool {
    opts.emit_anchors || opts.layout_prefix.is_some()
}

/// Text that is dot leaders alone: a run of
/// [`crate::textgrid::LEADER_DOTS`] dots or more, spaced or not, and nothing
/// else.
fn leaders_only(text: &str) -> bool {
    let mut dots = 0usize;
    for c in text.chars() {
        match c {
            c if c.is_whitespace() => {}
            '…' => dots += 3,
            '.' | '·' | '․' | '‥' => dots += 1,
            _ => return false,
        }
    }
    dots >= crate::textgrid::LEADER_DOTS
}

fn glyphs_outside_grids(glyphs: &[Glyph], grids: &[Grid]) -> Vec<Glyph> {
    // The glyphs each grid's cells read, so that a glyph is in a cell or
    // in the prose, never both and never neither (`Grid::takes`).
    let taken: Vec<Option<Vec<bool>>> = grids
        .iter()
        .map(|grid| match grid.layout.as_deref() {
            Some(lay) if !lay.claimed.is_empty() => None,
            _ => Some(grid.takes(glyphs)),
        })
        .collect();
    glyphs
        .iter()
        .enumerate()
        .filter(|(i, g)| {
            !grids
                .iter()
                .zip(&taken)
                .any(|(grid, taken)| match (grid.layout.as_deref(), taken) {
                    (_, Some(t)) => t[*i],
                    (Some(lay), None) => lay.claimed.binary_search(&g.draw_index).is_ok(),
                    (None, None) => false,
                })
        })
        .map(|(_, g)| g.clone())
        .collect()
}

/// Share of the page's width a reading spans, at the least, to reach across
/// a gutter between two of its columns: a table in one column of two is
/// under half the page wide, and one read across the gutter takes in most
/// of both columns. Narrower readings are not tested.
const ACROSS_COLUMNS: f64 = 0.4;

/// The page's two sides of the gutter a reading is set across, where it
/// crosses between two of the page's columns of text
/// ([`table::crosses_gutter`]): two tables set side by side, a table and the
/// prose beside it, a block of facts and a title. The gutter is one of the
/// channels of white running down through the reading (`table::channels_in`);
/// `runs` gives the page's runs of text, read once a channel parts a second
/// stub from the first.
///
/// A reading ruled through the gutter at its foot is one table
/// ([`ruled_across_gutter`]): a table set the page's full width over two
/// columns of prose parts its columns where the prose's gutter runs, and
/// the prose under it sets a paragraph either side; the rule under its
/// last row joins it.
fn crossed_gutter<'a>(
    g: &Grid,
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    width: f64,
    runs: impl Fn() -> &'a [table::TextRun] + Copy,
) -> Option<((f64, f64), (f64, f64))> {
    if g.bbox.x1 - g.bbox.x0 < width * ACROSS_COLUMNS {
        return None;
    }
    let rows = table::reading_rows(g, glyphs);
    table::channels_in(&rows).into_iter().find_map(|span| {
        let gutter = table::crosses_gutter(g, &rows, runs, span, width)?;
        (!ruled_across_gutter(g, glyphs, rules, gutter)).then_some(((0.0, gutter), (gutter, width)))
    })
}

/// Share of each side of the gutter, within a reading, that a rule run on
/// through the gutter reaches over: a table's rules run its width; a rule
/// under a heading in one column ends short of the gutter.
const ACROSS_GUTTER_REACH: f64 = 0.5;

/// Gap, in points, that parts one rule's pieces along a line: a rule drawn
/// a cell at a time meets itself end to end, and two tables side by side
/// leave the gutter's white between their rules.
const RULE_PIECE_GAP: f64 = 1.0;

/// Whether the reading is ruled through `gutter` as one table: a
/// horizontal rule runs on through the gutter, reaching
/// [`ACROSS_GUTTER_REACH`] of the way across the reading on both sides,
/// - **at both its ends**, within [`RULED_ROWS`] of its lines of its first
///   line and of its last, over, under or between them: a table ruled
///   between its rows, read a group of rows at a time; or
/// - **at its foot**, under its last line, within a line of it and nearer
///   it than the next text under it: a table's bottom rule under its last
///   group.
///
/// A rule drawn in pieces along one line counts as one where its pieces
/// meet within [`RULE_PIECE_GAP`]. A ruled table over a reading's first
/// rows only, the reading run on into the prose below it, is ruled at
/// neither its foot nor its end, and its last rows still cross between two
/// columns of text. Nor is a heading's rule over the reading, or the rule
/// over a page's foot set against the foot's own line.
fn ruled_across_gutter(
    g: &Grid,
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    gutter: f64,
) -> bool {
    let b = g.bbox;
    let line = (b.y1 - b.y0) / g.rows().max(1) as f64;
    let (left, right) = (
        gutter - (gutter - b.x0) * ACROSS_GUTTER_REACH,
        gutter + (b.x1 - gutter) * ACROSS_GUTTER_REACH,
    );
    // The lines on which a rule runs on through the gutter, near the reading.
    let mut lines: Vec<(f64, f64, f64)> = rules
        .iter()
        .filter(|r| {
            r.orientation == crate::rule::Orientation::Horizontal
                && r.axis_pos() >= b.y0 - RULED_ROWS * line
                && r.axis_pos() <= b.y1 + RULED_ROWS * line
        })
        .map(|r| (r.axis_pos(), r.bbox.x0, r.bbox.x1))
        .collect();
    lines.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
    let mut through: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // The pieces on one line, left to right.
        let mut on: Vec<(f64, f64)> = Vec::new();
        let axis = lines[i].0;
        while i < lines.len() && lines[i].0 - axis <= 1.0 {
            on.push((lines[i].1, lines[i].2));
            i += 1;
        }
        on.sort_by(|p, q| p.0.total_cmp(&q.0));
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for (x0, x1) in on {
            match runs.last_mut() {
                Some(r) if x0 <= r.1 + RULE_PIECE_GAP => r.1 = r.1.max(x1),
                _ => runs.push((x0, x1)),
            }
        }
        if runs.iter().any(|&(a, c)| a <= left && c >= right) {
            through.push(axis);
        }
    }
    let near = |end: f64| {
        through
            .iter()
            .any(|&y| (y - end).abs() <= RULED_ROWS * line)
    };
    if near(b.y0) && near(b.y1) {
        return true;
    }
    // The next text under the reading's foot, within its width.
    let next_under = |y: f64| {
        glyphs
            .iter()
            .filter(|q| !q.text.trim().is_empty())
            .filter_map(|q| q.bbox)
            .filter(|q| q.y0 >= y && q.x1 > b.x0 && q.x0 < b.x1)
            .map(|q| q.y0)
            .fold(f64::INFINITY, f64::min)
    };
    through
        .iter()
        .any(|&y| y >= b.y1 - line * 0.5 && y - b.y1 <= line && y - b.y1 <= next_under(y) - y)
}

/// How far from a reading's first and last lines, in its lines, a rule at
/// each end stands: a cell's padding over its first line, and under its
/// last the room a cell's last lines leave beside a taller neighbour.
const RULED_ROWS: f64 = 2.5;

/// Share of a cut's width a rule over or under it spans to rule it as a
/// table: a table's top and bottom rules run its full width, give or take
/// a cell's padding.
const RULED_ACROSS: f64 = 0.9;

/// A reading set across the page's gutter, cut at it: its columns between
/// `x0` and `x1`, on its rows, where two of them or more hold text. A side
/// read again on its own that yields no table is still the reading's table
/// there (a ruled table of words, which only the reading across the gutter
/// read in rows); a side left with one column, or ruled by nothing (a list
/// of references beside their numbers), is the page's prose. A cut's rows
/// are the reading's, less those the other side's text alone set.
fn side_of(
    g: &Grid,
    glyphs: &[Glyph],
    rules: &[crate::rule::Rule],
    x0: f64,
    x1: f64,
) -> Option<Grid> {
    let (lo, hi) = (g.bbox.x0.max(x0), g.bbox.x1.min(x1));
    let mut xs = vec![lo];
    xs.extend(
        g.xs.iter()
            .copied()
            .filter(|&x| x > lo + 1.0 && x < hi - 1.0),
    );
    xs.push(hi);
    let mut cut = Grid {
        xs,
        layout: None,
        bbox: crate::geom::BBox {
            x0: lo,
            x1: hi,
            ..g.bbox
        },
        ..g.clone()
    };
    // The rows the other side's text alone set are empty here: each goes
    // to the next row of this side's, as a table's ends are not found at
    // its first empty row (`trim_to_content`) inside the reading's.
    let texts = cut.cell_texts(glyphs);
    let cols = cut.cols();
    let used: Vec<usize> = (0..cut.rows())
        .filter(|&r| (0..cols).any(|c| !texts[r * cols + c].trim().is_empty()))
        .collect();
    let Some(&first) = used.first() else {
        return None;
    };
    let mut ys = vec![cut.ys[first]];
    ys.extend(used.iter().map(|&r| cut.ys[r + 1]));
    cut.ys = ys;
    cut.bbox.y0 = cut.ys[0];
    cut.bbox.y1 = cut.ys[cut.ys.len() - 1];
    cut.trim_to_content(glyphs);
    let texts = cut.cell_texts(glyphs);
    let cols = cut.cols();
    let held = (0..cols)
        .filter(|&c| (0..cut.rows()).any(|r| !texts[r * cols + c].trim().is_empty()))
        .count();
    // Ruled across its width over its rows or under them, as a table: a
    // column of the page's text, a list of references beside their
    // numbers, is not.
    // A rule may be drawn a piece a cell: the pieces along one line count
    // together.
    let b = cut.bbox;
    let reach = (b.y1 - b.y0) / cut.rows().max(1) as f64;
    let across = |y: f64| {
        let mut spans: Vec<(f64, f64)> = rules
            .iter()
            .filter(|r| {
                r.orientation == crate::rule::Orientation::Horizontal
                    && (r.axis_pos() - y).abs() <= 1.0
            })
            .map(|r| (r.bbox.x0.max(b.x0), r.bbox.x1.min(b.x1)))
            .filter(|(a, c)| c > a)
            .collect();
        spans.sort_by(|a, c| a.0.total_cmp(&c.0));
        let (mut covered, mut end) = (0.0, f64::MIN);
        for (a, c) in spans {
            let a = a.max(end);
            if c > a {
                covered += c - a;
                end = c;
            }
        }
        covered >= (b.x1 - b.x0) * RULED_ACROSS
    };
    let ruled = rules.iter().any(|r| {
        r.orientation == crate::rule::Orientation::Horizontal
            && ((r.axis_pos() - b.y0).abs() <= reach || (r.axis_pos() - b.y1).abs() <= reach)
            && across(r.axis_pos())
    });
    (cut.rows() >= 2 && held >= 2 && ruled).then_some(cut)
}

/// Alignment's tables, each read across the page's gutter between two of
/// its columns of text read again on each side of the gutter alone.
fn aligned_by_columns<'a>(
    found: Vec<Grid>,
    glyphs: &[Glyph],
    page: &crate::extract::Page,
    lines: impl Fn() -> &'a [table::TextRun] + Copy,
) -> Vec<Grid> {
    if found.is_empty() {
        return found;
    }
    let mut out = Vec::with_capacity(found.len());
    for g in found {
        let Some(sides) = crossed_gutter(&g, glyphs, &page.rules, page.width, lines) else {
            out.push(g);
            continue;
        };
        for (x0, x1) in [sides.0, sides.1] {
            let side: Vec<Glyph> = glyphs
                .iter()
                .filter(|q| {
                    q.bbox.is_some_and(|b| {
                        let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                        cx >= x0 && cx < x1 && cy >= g.bbox.y0 - 1.0 && cy <= g.bbox.y1 + 1.0
                    })
                })
                .cloned()
                .collect();
            let found: Vec<Grid> =
                table::detect_aligned(&side, &page.rules, &page.fills, page.index)
                    .into_iter()
                    .filter(|n| crossed_gutter(n, glyphs, &page.rules, page.width, lines).is_none())
                    .collect();
            if found.is_empty() {
                out.extend(side_of(&g, glyphs, &page.rules, x0, x1));
            }
            out.extend(found);
        }
    }
    out
}

pub fn analyze(raw: &mut RawDoc, outline: &[OutlineItem]) -> Analysis {
    analyze_with(raw, outline, &AnalyzeOptions::default())
}

/// Each page's route as [`analyze_with`] decides it: once glyphs drawn twice
/// at one spot (faux bold) are one, and a watermark laid over the text is
/// taken out, and before any other cleaning. Those two take glyphs off an
/// image: a label in faux bold or a watermark over a capture covers it as
/// drawn, and not once cleaned.
///
/// A region is named by its place in its page's route (`r0`, `r1`, …), and
/// an OCR reading is matched to its region by that name before the analysis
/// runs: routing the page as drawn there, and cleaned in the analysis, would
/// give one region two names. The document is not changed: the cleaning is
/// done on a copy ([`routing_pages`]).
pub fn region_routes(doc: &RawDoc) -> Vec<crate::route::Route> {
    routing_pages(doc)
        .iter()
        .map(|p| crate::route::decide(p).0)
        .collect()
}

/// A copy of the document's pages as [`analyze_with`] routes them: faux
/// bold counted once, a watermark laid over the text taken out. What an
/// OCR engine's regions are found on ([`crate::route::ocr_regions`]), so
/// they are numbered as the analysis's anchors are ([`region_routes`]).
pub fn routing_pages(doc: &RawDoc) -> Vec<crate::extract::Page> {
    let mut pages = doc.pages.clone();
    for p in pages.iter_mut() {
        dedup::remove_faux_bold(&mut p.glyphs, 8);
    }
    crate::watermark::strip_overlaid(&mut pages);
    pages
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
        // An accent drawn over its letter is part of it, once any copy of
        // either is gone: `e` and `´` are `é`.
        timed(&mut timings.dedup, || crate::accent::compose(&mut p.glyphs));
    }
    // A watermark laid over the text, before it is read into the lines it
    // crosses.
    timed(&mut timings.furniture, || {
        crate::watermark::strip_overlaid(&mut raw.pages)
    });
    // Routed here, after the two cleanings that take glyphs off an image and
    // before any other: the page an OCR reading numbers its regions on
    // ([`region_routes`]), so a region's anchor and its reading carry one
    // name. `Deterministic` is the right stand-in when nobody asks: both
    // consumers act only on `VlmRegions`, and both are unreachable in the
    // configurations that skip the call.
    let routes: Vec<crate::route::Route> = raw
        .pages
        .iter()
        .map(|p| {
            timed(&mut timings.routing, || {
                if routing_is_consulted(opts) {
                    crate::route::decide(p).0
                } else {
                    crate::route::Route::Deterministic
                }
            })
        })
        .collect();
    for (p, route) in raw.pages.iter_mut().zip(routes) {
        // A picture's lettering is read apart from the page's text, before
        // any reader of the page's text sees it.
        let (lettering, picture_tables) = take_lettering(p);
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
        // The page's glyphs are final from here: each grid's cells are read
        // from them once.
        let _cells = crate::cells::pin(&p.glyphs);

        // Tables first: a grid's glyphs must not also become paragraphs.
        // Two readings of the page's ruling, judged after trimming: a
        // page-wide grid can look richer before trim and collapse to a
        // handful of cells after it, which is exactly what happens on a form
        // of separate labelled boxes. Whichever survives trimming with more
        // populated cells is the one that explains the page.
        let trim = |mut gs: Vec<Grid>| -> Vec<Grid> {
            for g in gs.iter_mut() {
                g.trim_to_content_ruled(&p.glyphs, &p.rules);
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
            trim(table::detect_framed(
                &p.rules,
                &p.fills,
                &p.glyphs,
                p.index,
                &char_frames,
            ))
        });
        let mut grid_sources = vec![GridSource::Ruled; grids.len()];
        if !grids.is_empty() {
            let parts = timed(&mut timings.tables, || {
                trim(table::detect_by_component_framed(
                    &p.rules,
                    &p.fills,
                    &p.glyphs,
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

        // The page's lines as the text reader reads them, read once for the
        // readings below and for the runs of text `crossed_gutter` reads.
        let page_text = std::cell::OnceCell::new();
        let text_lines = || -> &[crate::textgrid::Line] {
            page_text.get_or_init(|| crate::textgrid::lines(&p.glyphs))
        };
        // A statement ruled by its figures' underlines rules off groups of
        // rows: each line of figures in a group is a row of its own.
        if grid_sources.contains(&GridSource::Ruled) {
            let underlined: Vec<usize> = (0..grids.len())
                .filter(|&k| {
                    grid_sources[k] == GridSource::Ruled
                        && table::ruled_by_underlines(&grids[k], &p.rules, &p.glyphs)
                })
                .collect();
            if !underlined.is_empty() {
                let lines = text_lines();
                for k in underlined {
                    crate::textgrid::split_underlined_bands(&mut grids[k], lines);
                }
            }
        }

        // The page's runs of text, for the tables read across a gutter
        // between two columns of text (`crossed_gutter`).
        let page_runs = std::cell::OnceCell::new();
        let col_lines = || -> &[table::TextRun] {
            page_runs.get_or_init(|| crate::textgrid::runs(text_lines()))
        };
        let page_aligned = timed(&mut timings.tables, || {
            let found = table::detect_aligned(&p.glyphs, &p.rules, &p.fills, p.index);
            aligned_by_columns(found, &p.glyphs, p, col_lines)
        });
        // Alignment is read again below from the glyphs outside the grids
        // (`glyphs_outside_grids`, which keeps them in order). Where no grid
        // took any, those are the page's glyphs as they are here, and so is
        // their reading.
        let aligned_in = |glyphs: &[Glyph]| {
            if glyphs.len() == p.glyphs.len() {
                page_aligned.clone()
            } else {
                let found = table::detect_aligned(glyphs, &p.rules, &p.fills, p.index);
                aligned_by_columns(found, glyphs, p, col_lines)
            }
        };
        let mut aligned_full = page_aligned.clone();
        // Justified prose lines its words up into columns; it is no reading
        // to prefer over a ruled table holding a paragraph in a cell.
        aligned_full.retain(|g| !table::reads_as_prose(g, &p.glyphs));
        let aligned_preferred = prefer_aligned_over_ruled(&grids, &aligned_full, &p.glyphs);
        // Where alignment reads the page's one coarse grid better, that grid
        // is still read from its text if it is ruled with horizontals only:
        // a journal's table between its top and bottom rules, a group's mid
        // rule across it, comes out of alignment in pieces (the rows under
        // one group, the header and the rest as text). Which reading stands
        // is settled below.
        let seeded_anyway = aligned_preferred && {
            let lines = text_lines();
            !crate::textgrid::unruled_grids(&grids, &p.rules, &p.fills, lines).is_empty()
        };
        // Tables ruled with horizontals only, or by nothing but the rules
        // under their figures, are read from their text: their bands are
        // not rows and their rules' ends are not columns. See `textgrid`.
        let mut beside_prose: Vec<crate::geom::BBox> = Vec::new();
        if !aligned_preferred || seeded_anyway {
            timed(&mut timings.tables, || {
                let lines = text_lines();
                let mut read: Vec<Grid> = Vec::new();
                let mut seeded: Vec<usize> = Vec::new();
                let debug = std::env::var_os("FDOC_TEXTGRID_DEBUG").is_some();
                let stacks = table::stack_columns(&p.rules, &p.fills);
                let unbroken = table::unbroken_stacks(&p.rules);
                let mut withheld: Vec<usize> = Vec::new();
                for i in crate::textgrid::unruled_grids(&grids, &p.rules, &p.fills, &lines) {
                    let beside =
                        crate::textgrid::ruled_beside_prose(&grids[i], &p.rules, &p.fills, &lines);
                    // The stacks of row lines ruled alike within the seed:
                    // the places their lines break, their columns' edges as
                    // drawn.
                    let within: Vec<&(crate::geom::BBox, Vec<f64>)> = stacks
                        .iter()
                        .filter(|(b, _)| {
                            overlap_area(b, &grids[i].bbox)
                                >= 0.5 * ((b.x1 - b.x0) * (b.y1 - b.y0)).max(1.0)
                        })
                        .collect();
                    for region in crate::textgrid::rule_regions(&p.rules, &grids[i].bbox) {
                        // A seed drawn from some of a stack's own lines reads
                        // as the stack does.
                        let in_stack = unbroken.iter().any(|(b, _)| {
                            region.x0 >= b.x0 - 1.0
                                && region.x1 <= b.x1 + 1.0
                                && region.y0 >= b.y0 - 1.0
                                && region.y1 <= b.y1 + 1.0
                        });
                        let mut got = if in_stack {
                            crate::textgrid::read_ruled_stack(&lines, &p.rules, region, p.index)
                        } else {
                            crate::textgrid::read(&lines, &p.rules, region, p.index)
                        };
                        // Where alignment was preferred, a seed whose last
                        // region holds the header alone (a top rule and the
                        // rule under the header, the table run on past the
                        // page's foot with no rule under it) is read on below
                        // that rule: its rows, and its group labels among
                        // them, that alignment reads in pieces. Which reading
                        // stands is settled below, as for any seed.
                        if got.is_empty()
                            && seeded_anyway
                            && (region.y1 - grids[i].bbox.y1).abs() <= 3.0
                        {
                            let below = crate::geom::BBox {
                                y1: p.height,
                                ..region
                            };
                            got = crate::textgrid::read(&lines, &p.rules, below, p.index);
                            if debug {
                                eprintln!(
                                    "TEXTGRID p{} seed read on below {:?} -> {:?}",
                                    p.index,
                                    below,
                                    got.iter()
                                        .map(|g| (g.rows(), g.cols(), g.bbox))
                                        .collect::<Vec<_>>()
                                );
                            }
                        }
                        // A reading across the columns a stack's rules draw
                        // is no table: neither it nor the seed stands, and
                        // its text stays text.
                        let (merged, got): (Vec<Grid>, Vec<Grid>) =
                            got.into_iter().partition(|g| {
                                within.iter().any(|(b, edges)| {
                                    crate::textgrid::merges_drawn_columns(g, edges, b, &p.glyphs)
                                })
                            });
                        if !merged.is_empty() && !withheld.contains(&i) {
                            withheld.push(i);
                        }
                        if beside {
                            beside_prose.extend(got.iter().map(|g| g.bbox));
                        }
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
                // Row lines drawn unbroken across one stretch, four or more
                // at an even pitch, beside another such stack: two tables
                // ruled with horizontals alone, set side by side in a page's
                // columns. Their lines interleave or fall level, and the
                // page-wide reading takes neither or both as one. Each is
                // read from its text, where all of these hold:
                // - no grid already holds most of it: a table read another
                //   way keeps that reading;
                // - its bands hold text ([`crate::textgrid::bands_hold_text`]):
                //   a chart's gridlines stand over bars, a drawing's over
                //   nothing;
                // - the reading stands within its lines: a stack narrower
                //   than the table it rules is one column's lines in a wider
                //   table. A twentieth of its width is left for a header
                //   label hung past them.
                let stands_beside = |r: &crate::geom::BBox| {
                    unbroken.iter().map(|(o, _)| o).any(|o| {
                        let shared = o.y1.min(r.y1) - o.y0.max(r.y0);
                        (o.x1 <= r.x0 || o.x0 >= r.x1)
                            && shared > 0.0
                            && shared >= 0.5 * (o.y1 - o.y0).min(r.y1 - r.y0)
                    })
                };
                for (region, ys) in &unbroken {
                    let region = *region;
                    let area = ((region.x1 - region.x0) * (region.y1 - region.y0)).max(1.0);
                    if grids
                        .iter()
                        .any(|g| overlap_area(&g.bbox, &region) >= 0.5 * area)
                        || !stands_beside(&region)
                        || !crate::textgrid::bands_hold_text(&lines, region, ys)
                    {
                        continue;
                    }
                    let mut got =
                        crate::textgrid::read_ruled_stack(&lines, &p.rules, region, p.index);
                    let slack = 0.05 * (region.x1 - region.x0);
                    got.retain(|g| {
                        g.bbox.x0 >= region.x0 - slack && g.bbox.x1 <= region.x1 + slack
                    });
                    if debug {
                        eprintln!(
                            "TEXTGRID p{} unbroken stack {:?} -> {:?}",
                            p.index,
                            region,
                            got.iter()
                                .map(|g| (g.rows(), g.cols(), g.bbox))
                                .collect::<Vec<_>>()
                        );
                    }
                    read.extend(got);
                }
                // Runs of figures no grid holds: a statement ruled by
                // nothing but the underlines under its figures. Not where
                // alignment finds a layout its type names (a transposed
                // table, a field list), which it reads better.
                let mut taken: Vec<crate::geom::BBox> = grids.iter().map(|g| g.bbox).collect();
                taken.extend(read.iter().map(|g| g.bbox));
                let outside = glyphs_outside_grids(&p.glyphs, &grids);
                let _outside_cells = crate::cells::pin(&outside);
                let accepted = aligned_in(&outside);
                let typed: Vec<crate::geom::BBox> = accepted
                    .iter()
                    .filter(|g| table::typed_layout(g, &outside))
                    .map(|g| g.bbox)
                    .collect();
                // Where alignment was preferred, only the seed is read again:
                // alignment reads the runs of figures there.
                for region in crate::textgrid::figure_runs(&lines, &p.rules, &taken)
                    .into_iter()
                    .filter(|_| !aligned_preferred)
                {
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
                // A reading set across the page's gutter, between two of its
                // columns of text, is no table: each side is read again on
                // its own, the next column's text beside it left out. Before
                // the larger reading is kept, so a table's own reading in its
                // column is not lost to one that runs on across the gutter.
                {
                    let mut again: Vec<Grid> = Vec::new();
                    read.retain(|g| {
                        let Some(sides) =
                            crossed_gutter(g, &p.glyphs, &p.rules, p.width, col_lines)
                        else {
                            return true;
                        };
                        for (x0, x1) in [sides.0, sides.1] {
                            let side = crate::textgrid::lines_within(&p.glyphs, x0, x1);
                            let region = crate::geom::BBox {
                                x0: g.bbox.x0.max(x0),
                                x1: g.bbox.x1.min(x1),
                                ..g.bbox
                            };
                            let found: Vec<Grid> =
                                crate::textgrid::read(&side, &p.rules, region, p.index)
                                    .into_iter()
                                    .filter(|n| {
                                        crossed_gutter(n, &p.glyphs, &p.rules, p.width, col_lines)
                                            .is_none()
                                    })
                                    .collect();
                            if found.is_empty() {
                                again.extend(side_of(g, &p.glyphs, &p.rules, x0, x1));
                            }
                            again.extend(found);
                        }
                        false
                    });
                    read.extend(again);
                }
                // One table found from two seeds is kept once, the larger.
                read.sort_by_key(|g| std::cmp::Reverse(g.rows() * g.cols()));
                let mut kept: Vec<Grid> = Vec::new();
                for g in read {
                    if !kept.iter().any(|k| overlap_area(&k.bbox, &g.bbox) > 0.0) {
                        kept.push(g);
                    }
                }
                if kept.is_empty() && withheld.is_empty() {
                    return;
                }
                let mut out_grids = Vec::new();
                let mut out_sources = Vec::new();
                for (gi, (g, s)) in grids.drain(..).zip(grid_sources.drain(..)).enumerate() {
                    let area = ((g.bbox.x1 - g.bbox.x0) * (g.bbox.y1 - g.bbox.y0)).max(1.0);
                    // A seed that was read is replaced by its reading; another
                    // grid only where a reading covers much of it.
                    let replaced = seeded.contains(&gi)
                        || withheld.contains(&gi)
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
        let mut read_from_text = grid_sources.contains(&GridSource::Text);
        // Where alignment was preferred, the seed's reading from its text
        // stands only where it holds more of the table than alignment does:
        // both read the same table, and the better leaves less of it out.
        if read_from_text && seeded_anyway {
            let held = |gs: &[Grid]| -> usize {
                gs.iter()
                    .flat_map(|g| g.cell_texts(&p.glyphs))
                    .map(|t| t.chars().filter(|c| !c.is_whitespace()).count())
                    .sum()
            };
            if held(&grids) <= held(&aligned_full) {
                read_from_text = false;
                beside_prose.clear();
            }
        }

        if read_from_text {
        } else if aligned_preferred {
            grids = aligned_full;
            grid_sources = vec![GridSource::Aligned; grids.len()];
        } else {
            let mut horizontal_bands = timed(&mut timings.tables, || {
                table::detect_horizontal_bands(&p.glyphs, &p.rules, p.index)
            });
            // Nor a reading that cuts a ruled table's paragraph into words.
            horizontal_bands.retain(|g| {
                !table::reads_as_prose(g, &p.glyphs) && !table::cuts_words(g, &p.glyphs)
            });
            if prefer_horizontal_bands(&grids, &horizontal_bands, &p.glyphs) {
                grids = horizontal_bands;
                grid_sources = vec![GridSource::HorizontalBand; grids.len()];
            }
        }

        // A header set as a shaded band over a table read under it, from
        // its text or its alignment, is that table's header row.
        {
            let bands = table::header_bands(&p.rules, &p.fills);
            if !bands.is_empty() {
                let keep = table::take_header_bands(&mut grids, &bands, &p.glyphs);
                let mut k = keep.iter();
                grids.retain(|_| *k.next().unwrap_or(&true));
                let mut k = keep.iter();
                grid_sources.retain(|_| *k.next().unwrap_or(&true));
            }
        }

        // A table holds two columns of content or more, as read here or as
        // its text's alignment parts it (`Grid::split_by_alignment`, below).
        // One left with one is a box drawn round prose, where it holds a row
        // or two (a paragraph and a banner between a rule's marks, a
        // figure's caption) or its rows run as sentences do: its text is the
        // page's prose. A column of short entries, one a row, is a list the
        // table keeps (`Np Number of reputation tables…`).
        {
            let tabular: Vec<bool> = grids
                .iter()
                .map(|g| {
                    let filled = |g: &Grid| {
                        let cols = g.cols();
                        let texts = g.cell_texts(&p.glyphs);
                        (0..cols)
                            .filter(|&c| {
                                (0..g.rows()).any(|r| !texts[r * cols + c].trim().is_empty())
                            })
                            .count()
                    };
                    let prose = || {
                        let texts = g.cell_texts(&p.glyphs);
                        let rows: Vec<&String> =
                            texts.iter().filter(|t| !t.trim().is_empty()).collect();
                        let sentences = rows
                            .iter()
                            .filter(|t| t.split_whitespace().count() >= SENTENCE_WORDS)
                            .count();
                        rows.len() <= 2 || sentences * 2 > rows.len()
                    };
                    filled(g) >= 2 || !prose() || {
                        let mut split = g.clone();
                        split.split_by_alignment(&p.glyphs);
                        filled(&split) >= 2
                    }
                })
                .collect();
            let mut k = tabular.iter();
            grids.retain(|_| *k.next().unwrap_or(&true));
            let mut k = tabular.iter();
            grid_sources.retain(|_| *k.next().unwrap_or(&true));
        }

        // The labels a ruled table sets over the rules that group its
        // columns are its top row. Taken in once the readings above are
        // chosen: to them the rules under the labels are the table's top
        // edge, not a boundary between its rows ruled short.
        for (g, source) in grids.iter_mut().zip(&grid_sources) {
            if *source == GridSource::Ruled {
                g.take_group_labels(&p.rules, &p.glyphs);
                g.take_column_heads(&p.rules, &p.glyphs);
            }
        }

        // Columns of prose and boxes laid out around paragraphs are not
        // tables: their glyphs go back to the prose, read column by column.
        {
            let keep: Vec<bool> = grids
                .iter()
                .map(|g| !table::reads_as_prose(g, &p.glyphs))
                .collect();
            let mut k = keep.iter();
            grids.retain(|_| *k.next().unwrap_or(&true));
            let mut k = keep.iter();
            grid_sources.retain(|_| *k.next().unwrap_or(&true));
        }
        let prose = timed(&mut timings.columns, || {
            glyphs_outside_grids(&p.glyphs, &grids)
        });
        let _prose_cells = crate::cells::pin(&prose);

        // The prose's columns, which the field lists below look between too.
        // A table ruled under its rows with horizontals alone, standing
        // beside a column of the page's prose (`textgrid::ruled_beside_prose`),
        // is set in a column of its own: its text keeps that column's extent,
        // as it would read had it been prose.
        //
        // The test is pure geometry, made on the ruled grid that seeds the
        // table read from its text. In practice only such seeds give tables
        // of this shape. Made on every table the page's readers keep, it
        // changed two documents more, one for the better and one for the
        // worse; counted, a table's own gutters part the page's text at them.
        let prose_cols = timed(&mut timings.columns, || {
            let elsewhere: Vec<Grid> = grids
                .iter()
                .zip(&grid_sources)
                .filter(|(g, s)| !(**s == GridSource::Text && beside_prose.contains(&g.bbox)))
                .map(|(g, _)| g.clone())
                .collect();
            if elsewhere.len() == grids.len() {
                crate::column::detect(&prose)
            } else {
                crate::column::detect(&glyphs_outside_grids(&p.glyphs, &elsewhere))
            }
        });
        let mut cols = timed(&mut timings.columns, || {
            line::assemble_columns_in(&prose, &p.rules, p.index, &prose_cols)
        });

        // Second table strategy: alignment, for the 14 corpus documents whose
        // tables have no vertical rules at all and so are invisible to the
        // ruled detector. Gated on corroborating drawn geometry — see
        // `table::is_corroborated`. An earlier ungated version was
        // net-negative: it converted prose into tables and cost more in reading
        // order (NID 0.868 -> 0.839) than it gained in table structure.
        let mut aligned = timed(&mut timings.tables, || aligned_in(&prose));
        // A box drawn round a drawing parts the text either side of it: a
        // grid read across its edge put a diagram's labels and the note in
        // the column beside its box in one row.
        let walls = table::walls(&p.fills);
        // Its glyphs are the prose's, outside every grid; but a run of them
        // either side of a grid can line up into a table laid over it.
        aligned.retain(|g| {
            g.rows() >= 2
                && g.cols() >= 2
                && !grids.iter().any(|o| overlap_area(&o.bbox, &g.bbox) > 0.0)
                && !table::reads_as_prose(g, &prose)
                && !table::walled_in(&g.bbox, &walls)
                && !lists_its_rows(g, &prose)
        });
        // Field lists set without rules, found between the page's gutters
        // where a row across the page holds more than a label and its
        // value. Their cells hold the prose's glyphs, so they are pointed
        // at the page's.
        let (fields, column_tables) = timed(&mut timings.tables, || {
            let text = table::PageRows::new(&prose);
            (
                table::field_lists_in(&prose, p.index, &prose_cols, &text),
                table::column_tables_in(&prose, p.index, &prose_cols, &text),
            )
        });
        let field_count = fields.len();
        let fields: Vec<Grid> = fields.into_iter().chain(column_tables).collect();
        // Whether a table was read within the page's columns: then its
        // columns stand once it is taken out.
        let mut in_columns = false;
        if !fields.is_empty() {
            let at: std::collections::HashMap<usize, usize> = p
                .glyphs
                .iter()
                .enumerate()
                .map(|(i, g)| (g.draw_index, i))
                .collect();
            for (fi, mut f) in fields.into_iter().enumerate() {
                if grids
                    .iter()
                    .chain(aligned.iter())
                    .any(|o| overlap_area(&o.bbox, &f.bbox) > 0.0)
                    || table::walled_in(&f.bbox, &walls)
                {
                    continue;
                }
                let Some(lay) = f.layout.as_mut() else {
                    continue;
                };
                let lay = std::sync::Arc::make_mut(lay);
                let mut mapped = true;
                for i in lay.cells.iter_mut().flatten() {
                    match at.get(&prose[*i].draw_index) {
                        Some(&k) => *i = k,
                        None => mapped = false,
                    }
                }
                if mapped {
                    in_columns |= fi >= field_count;
                    aligned.push(f);
                }
            }
        }
        if !aligned.is_empty() {
            // Withhold their glyphs from prose too, and re-assemble what is left.
            // A table read between the page's gutters leaves them where they
            // were: found again without its rows, they can fall elsewhere, and
            // the columns' lines would be read across each other.
            let remaining = glyphs_outside_grids(&prose, &aligned);
            cols = timed(&mut timings.columns, || {
                if in_columns {
                    line::assemble_columns_in(&remaining, &p.rules, p.index, &prose_cols)
                } else {
                    line::assemble_columns_with_rules(&remaining, &p.rules, p.index)
                }
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

        // A chart's scale beside the page's text is no table: a column of
        // numbers each set level with a tick of the axis, and at most one
        // other column holding text, the column of prose the chart stands
        // beside (`spending have deteriorated. Business | 18.0`) or the
        // chart's own legend. Its lines are the page's prose, and the
        // scale is the chart's.
        {
            let n = grids.len();
            let mut kept_grids = Vec::with_capacity(n);
            let mut kept_sources = Vec::with_capacity(n);
            for (grid, source) in grids.into_iter().zip(grid_sources) {
                if scale_beside_text(&grid, &p.glyphs, &p.rules, &p.ticks) {
                    continue;
                }
                kept_grids.push(grid);
                kept_sources.push(source);
            }
            grids = kept_grids;
            grid_sources = kept_sources;
            if grids.len() != n {
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

        // A line of dot leaders alone, inside a table's box, is the gutter
        // between a label the table reads and its value, left between their
        // cells: none of the page's text. Kept among the page's glyphs until
        // here, it measured the page's word spaces with them.
        for col in cols.iter_mut() {
            col.retain(|l| {
                let (x, y) = ((l.bbox.x0 + l.bbox.x1) / 2.0, (l.bbox.y0 + l.bbox.y1) / 2.0);
                !(leaders_only(&l.text)
                    && grids.iter().any(|g| {
                        x >= g.bbox.x0 && x <= g.bbox.x1 && y >= g.bbox.y0 && y <= g.bbox.y1
                    }))
            });
        }

        // Each region's columns in turn, down the page.
        let table_boxes: Vec<crate::geom::BBox> = grids.iter().map(|g| g.bbox).collect();
        let pictures: Vec<crate::geom::BBox> = p.images.iter().map(|im| im.bbox).collect();
        // Drawn boxes, outlined or filled: a stretch's lines read box by
        // box where they stand in boxes side by side.
        let frames: Vec<crate::geom::BBox> = p.fills.iter().map(|f| f.bbox).collect();
        let groups = crate::region::read_order(cols, &p.rules, &table_boxes, &pictures, &frames);
        let prose_sources: Vec<(usize, bool)> =
            groups.iter().map(|g| (g.column, g.own_measure)).collect();
        let cols: Vec<Vec<Line>> = groups.into_iter().map(|g| g.lines).collect();
        // The lettering's lines are weighed as furniture with the page's:
        // a logo set at one place page after page is the page's furniture
        // however it was read. They follow the page's own lines.
        flat_lines.push((
            cols.iter()
                .flatten()
                .chain(lettering.iter().flat_map(|l| &l.lines))
                .cloned()
                .collect(),
            p.height,
        ));
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
            prose_sources,
            route,
            missing_tables: missing,
            demoted_tables: demoted,
            lettering,
            picture_tables,
        });
    }

    // The page each bookmark lands on, and its title: a running head the
    // outline names is set as its section's heading there.
    let bookmarks: Vec<(usize, String)> = outline
        .iter()
        .filter_map(|it| it.page.map(|pi| (pi, it.title.clone())))
        .collect();
    let widths: Vec<f64> = raw.pages.iter().map(|p| p.width).collect();
    // Each page's drawn shapes, by which a chart's legend and axis labels
    // are told from running lines (see [`furniture::in_drawing`]).
    let drawings: Vec<furniture::Drawing> = raw
        .pages
        .iter()
        .zip(&layouts)
        .map(|(p, lay)| furniture::Drawing {
            tables: lay.grids.iter().map(|g| g.bbox).collect(),
            shapes: p
                .fills
                .iter()
                .map(|f| f.bbox)
                .chain(p.rules.iter().map(|r| r.bbox))
                .chain(p.traces.iter().map(|t| t.bbox))
                .collect(),
            ticks: p.ticks.iter().map(|t| t.bbox).collect(),
        })
        .collect();
    let (mut marks, exact, kept_heads) = timed(&mut timings.furniture, || {
        furniture::detect_with_heads(&flat_lines, &bookmarks, &widths, &drawings)
    });
    // A line OCR read is never furniture. Repetition is furniture's only
    // evidence, and on a scan the same content repeats for its own reasons: a
    // fax that sends one ID card on every page, a form copied per applicant.
    // Marked, its text would leave the body and its pattern be scrubbed from
    // every table cell. A page number left in the body costs a line.
    for (pi, page_marks) in marks.iter_mut().enumerate() {
        let page = &raw.pages[pi];
        if page.ocr.is_none() {
            continue;
        }
        // By the ink inside the line's box: a line's glyph indices point into
        // the glyphs it was assembled from (the page's outside its grids),
        // not into the page's.
        page_marks.retain(|li, _| {
            let b = flat_lines[pi].0[*li].bbox;
            let (mut ocr, mut all) = (0usize, 0usize);
            for g in &page.glyphs {
                let Some(gb) = g.bbox else { continue };
                if g.text.trim().is_empty()
                    || !b.contains((gb.x0 + gb.x1) * 0.5, (gb.y0 + gb.y1) * 0.5)
                {
                    continue;
                }
                all += 1;
                ocr += usize::from(page.is_ocr(g));
            }
            all == 0 || ocr * 2 < all
        });
    }
    // The texts kept as a section's heading on their page, though the same
    // words run as furniture on the pages after: never scrubbed there.
    let kept_heads: Vec<std::collections::HashSet<String>> = kept_heads
        .iter()
        .enumerate()
        .map(|(pi, set)| {
            set.iter()
                .map(|li| flat_lines[pi].0[*li].text.trim().to_string())
                .collect()
        })
        .collect();
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
        furniture::detect_folio_places(&edges, &widths)
    });
    let (folios, folio_places): (Vec<Option<String>>, Vec<Option<f64>>) =
        folios.into_iter().unzip();
    // A page's folio standing as a line of its own where the folio's line
    // stands is the page's number, though too few pages left it among
    // their lines for repetition to tell: a table reaching the foot takes it
    // on the others, and the first and last pages kept theirs in the body.
    for (pi, folio) in folios.iter().enumerate() {
        let (Some(folio), Some(place), Some((lines, height))) =
            (folio.as_deref(), folio_places[pi], flat_lines.get(pi))
        else {
            continue;
        };
        if *height <= 0.0 {
            continue;
        }
        for (li, l) in lines.iter().enumerate() {
            if furniture::is_folio_line(&l.text, folio, l.bbox.y0 / height, place) {
                marks[pi]
                    .entry(li)
                    .or_insert(furniture::Furniture::PageNumber);
            }
        }
    }
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
    // The rows each furniture text runs on, as shares of the page's height:
    // a running line leaks into a block on its own row, where a line the
    // page set beside it merged with it. Elsewhere its words are content: a
    // running head parted into its groups (`July 26, 2023`, `Chair Powell's
    // Press Conference`) is set again in the transcript's title under it.
    let mut furniture_rows: std::collections::HashMap<String, Vec<(f64, f64)>> = Default::default();
    for (pi, page_marks) in marks.iter().enumerate() {
        let (lines, height) = &flat_lines[pi];
        if *height <= 0.0 {
            continue;
        }
        for (li, kind) in page_marks.iter() {
            let l = &lines[*li];
            furniture_rows
                .entry(furniture_key(
                    l.text.trim(),
                    matches!(kind, furniture::Furniture::PageNumber),
                ))
                .or_default()
                .push((l.bbox.y0 / height, l.bbox.y1 / height));
        }
    }
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
    // The running strings to scrub from text spanning `top..foot` of its
    // page, as shares of the page's height: those found on those rows.
    let on_rows = |top: f64, foot: f64| -> Vec<(String, bool)> {
        scrub_texts
            .iter()
            .filter(|(t, vary)| {
                furniture_rows
                    .get(&furniture_key(t, *vary))
                    .is_none_or(|rows| {
                        rows.iter().any(|&(y0, y1)| {
                            y0 < foot + FURNITURE_ROW_SLACK && top < y1 + FURNITURE_ROW_SLACK
                        })
                    })
            })
            .cloned()
            .collect()
    };
    let bare: Vec<Vec<Line>> = flat_lines.iter().map(|(l, _)| l.clone()).collect();
    let leading = block::modal_leading(&bare);
    {
        let mut lines: Vec<&mut Line> = layouts
            .iter_mut()
            .flat_map(|l| l.prose_columns.iter_mut().flatten())
            .collect();
        crate::line::mark_compounds(&mut lines);
    }

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
        // The region pass reads a page column in stretches. A stretch's
        // measure is its column's, unless it is one side of a box read at
        // its own gutter; and side heads are read beside their text over
        // whole columns, as the page-wide pass found them.
        let sources = &layout.prose_sources;
        let measures: Vec<Vec<Line>> = (0..kept_columns.len())
            .map(|i| match sources.get(i) {
                Some(&(c, false)) => kept_columns
                    .iter()
                    .zip(sources)
                    .filter(|(_, s)| s.0 == c)
                    .flat_map(|(k, _)| k.iter().cloned())
                    .collect(),
                _ => kept_columns[i].clone(),
            })
            .collect();
        let page_columns = {
            let mut c: Vec<usize> = sources.iter().map(|s| s.0).collect();
            c.sort_unstable();
            c.dedup();
            c.len()
        };
        let contents_layout = page_columns > 1 && is_contents_layout(&kept_columns);
        let checkboxes = crate::rule::checkboxes(&raw.pages[pi].fills);
        let dots = block::marking_dots(
            &kept_columns.iter().flatten().cloned().collect::<Vec<_>>(),
            &raw.pages[pi].dots,
        );
        let bands: Vec<crate::geom::BBox> = raw.pages[pi].fills.iter().map(|f| f.bbox).collect();
        let ocr = block::ocr_centers(&raw.pages[pi]);
        let body = block::body_size(kept_columns.iter().flatten());
        let tables: Vec<crate::geom::BBox> = layout.grids.iter().map(|g| g.bbox).collect();
        let block_columns: Vec<Vec<Block>> = timed(&mut timings.blocks, || {
            let mut runs: Vec<(usize, Vec<Block>)> = Vec::new();
            for (i, col) in kept_columns.iter().enumerate() {
                let blocks = block::assemble_in_measure(
                    col,
                    &measures[i],
                    leading,
                    &checkboxes,
                    &dots,
                    &bands,
                    body,
                    &ocr,
                );
                let c = sources.get(i).map_or(i, |s| s.0);
                match runs.last_mut() {
                    Some((last, run)) if *last == c => run.extend(blocks),
                    _ => runs.push((c, blocks)),
                }
            }
            block::join_side_labels(block::read_side_heads(
                runs.into_iter().map(|(_, r)| r).collect(),
                &tables,
            ))
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
        // Each picture's lettering, read into blocks of its own, stands where
        // the picture does: in the lane of the blocks it lies across, after
        // the last of them wholly above it. Lines the furniture pass marked
        // are left out, as the page's are.
        let mut idx = kept_columns_len(cols);
        // Pictures at the page's top, over every block, go first, in order
        // down and across: a page's logos over its heading.
        let mut tops = 0usize;
        let mut order: Vec<(usize, &Lettering)> = Vec::new();
        for pic in &layout.lettering {
            order.push((idx, pic));
            idx += pic.lines.len();
        }
        order.sort_by(|a, b| {
            a.1.bbox
                .y0
                .total_cmp(&b.1.bbox.y0)
                .then(a.1.bbox.x0.total_cmp(&b.1.bbox.x0))
        });
        for (idx, pic) in order {
            let lines: Vec<Line> = pic
                .lines
                .iter()
                .enumerate()
                .filter(|(k, _)| !marks[pi].contains_key(&(idx + k)))
                .map(|(_, l)| l.clone())
                .collect();
            if lines.is_empty() {
                continue;
            }
            let blocks = picture_blocks(pic, &lines, leading);
            let n = blocks.len();
            let lane_of =
                |b: &crate::geom::BBox| b.x1.min(pic.bbox.x1) - b.x0.max(pic.bbox.x0) > 0.0;
            let lane = |b: &Block| lane_of(&b.bbox);
            let page_top = !out
                .iter()
                .any(|b| b.bbox.y1 <= pic.bbox.y0 + 1.0 && b.lines.iter().all(|l| l.ocr.is_none()));
            let over = |b: &Block| b.bbox.y1 <= pic.bbox.y0 + 1.0;
            let at = if page_top {
                tops
            } else {
                let mut at = out
                    .iter()
                    .rposition(|b| lane(b) && over(b))
                    .map(|i| i + 1)
                    .or_else(|| out.iter().position(|b| lane(b) && b.bbox.y0 >= pic.bbox.y0))
                    .unwrap_or(out.len());
                // Blocks read next that stand over the picture outside its
                // lane, with nothing under them in their own column reaching
                // down beside it, are read before it: a footnote set as an
                // image under two of three columns follows the third.
                while at < out.len() && over(&out[at]) {
                    let b = out[at].bbox;
                    let beside = out.iter().any(|c| {
                        c.bbox.x1.min(b.x1) - c.bbox.x0.max(b.x0) > 0.0
                            && c.bbox.y0 >= b.y1 - 1.0
                            && !over(c)
                    });
                    if beside {
                        break;
                    }
                    at += 1;
                }
                // A table over the picture in its lane is read before it.
                // Tables join the blocks after this, each after the last
                // block in its lane wholly above it ([`interleave_tables`]),
                // so the picture goes after that block too: a chart under a
                // table follows the table, not its title.
                for (g, _) in
                    layout
                        .grids
                        .iter()
                        .zip(&layout.demoted_tables)
                        .filter(|(g, demoted)| {
                            !**demoted && lane_of(&g.bbox) && g.bbox.y1 <= pic.bbox.y0 + 1.0
                        })
                {
                    let t = g.bbox;
                    if let Some(i) = out.iter().rposition(|b| {
                        b.bbox.x1.min(t.x1) - b.bbox.x0.max(t.x0) > 0.0 && b.bbox.y1 <= t.y0
                    }) {
                        at = at.max(i + 1);
                    }
                }
                at
            };
            if page_top {
                tops += n;
            }
            out.splice(at..at, blocks);
        }
        blocks_per_page.push(out);
    }

    // Each page's figures, and the figure each block belongs to: worked out
    // before headings are, since the text of drawings is no measure of the
    // body's size, and before any element is typed, since whether a heading
    // yields to a figure depends on how the document sets its headings
    // elsewhere. Gathering a numbered list rewrites the blocks, so the
    // figures follow it.
    let mut page_figures = find_figures(&raw.pages, &blocks_per_page, &layouts);
    let mut body_font = heading::body_font_size_outside(&blocks_per_page, &drawn(&page_figures));
    let thin = thin_prose_body(&blocks_per_page, &raw.pages, &layouts);
    if let Some(size) = thin {
        body_font = size;
    }
    let body_bold = heading::body_is_bold(raw.pages.iter().flat_map(|p| p.glyphs.iter()));
    let (headings, kickers) = timed(&mut timings.headings, || {
        let first = heading::detect_with_kickers(&blocks_per_page, outline, body_font, body_bold);
        let named = heading::outline_names(outline);
        let in_section = heading::in_section(&first.0);
        if block::gather_counted_items(&mut blocks_per_page, &|t| named(t), &in_section) {
            page_figures = find_figures(&raw.pages, &blocks_per_page, &layouts);
            body_font = thin.unwrap_or_else(|| {
                heading::body_font_size_outside(&blocks_per_page, &drawn(&page_figures))
            });
            heading::detect_with_kickers(&blocks_per_page, outline, body_font, body_bold)
        } else {
            first
        }
    });
    // The first page's kickers, by block: text over the title.
    let kickers: std::collections::HashSet<(usize, usize)> =
        kickers.into_iter().map(|b| (0, b)).collect();

    // Heading lookup by source block. Text is not an identity: repeated chart
    // labels and form fields can have identical text on one page while only
    // one occurrence is structurally a heading.
    let mut is_heading: std::collections::HashMap<(usize, usize), (usize, Evidence)> =
        Default::default();
    let mut form_text: std::collections::HashSet<(usize, usize)> = Default::default();
    let level_info: std::collections::HashMap<(usize, usize), heading::LevelInfo> = headings
        .iter()
        .map(|h| ((h.page, h.block_index), h.level_info()))
        .collect();
    for h in &headings {
        is_heading.insert((h.page, h.block_index), (h.level, h.evidence));
        if h.form_text {
            form_text.insert((h.page, h.block_index));
        }
    }

    let mut elements = Vec::new();
    let ruled_tables = layouts
        .iter()
        .flat_map(|l| l.grid_sources.iter())
        .filter(|s| **s == GridSource::Ruled)
        .count();

    let mut n = 0usize;
    // Each heading's block, by its element's id.
    let mut heading_block: std::collections::HashMap<String, (usize, usize)> = Default::default();
    let mut tables = 0usize;
    // Each table's column edges and whether it opens with a styled row,
    // for joining a table the page break split.
    let mut frames: Vec<TableFrame> = Vec::new();
    // The styles the document sets its headings in away from any figure.
    // A heading in one of them that a figure took as a label is the
    // document's heading, set near a drawing — `Operation` over a drawing
    // of the control it describes, a side-head level with a chair's — and
    // not a chart's title or legend, whose style is the chart's own.
    let heading_style = |b: &Block| {
        (
            (b.font_size * 2.0).round() as i32,
            b.lines.iter().all(|l| l.bold),
        )
    };
    let mut free_styles: std::collections::HashSet<(i32, bool)> = Default::default();
    for (pi, blocks) in blocks_per_page.iter().enumerate() {
        for (bi, b) in blocks.iter().enumerate() {
            if is_heading.contains_key(&(pi, bi)) && page_figures[pi].1[bi].is_none() {
                free_styles.insert(heading_style(b));
            }
        }
    }
    // A heading the template sets again over a later page, line for line,
    // read there as running, is set in its style away from any figure: a
    // catalogue's banner over its first page, beside a drawing, and over
    // the facing page as its head.
    let heading_texts: std::collections::HashSet<String> = is_heading
        .keys()
        .flat_map(|&(pi, bi)| blocks_per_page[pi][bi].lines.iter())
        .map(|l| l.text.trim().to_string())
        .collect();
    for (pi, page_marks) in marks.iter().enumerate() {
        for (li, kind) in page_marks {
            let l = &flat_lines[pi].0[*li];
            if *kind == furniture::Furniture::Header && heading_texts.contains(l.text.trim()) {
                free_styles.insert(((l.font_size * 2.0).round() as i32, l.bold));
            }
        }
    }
    for (pi, blocks) in blocks_per_page.iter().enumerate() {
        let _cells = crate::cells::pin(&raw.pages[pi].glyphs);
        let mut page_elements = Vec::new();
        // Beside each element, a list item's marker and text start, its
        // size, and whether its first line carries on the item before it.
        let mut item_text: Vec<Option<ListMark>> = Vec::new();
        let (figures, member) = &page_figures[pi];
        let in_figure = |i: usize| member[i];
        for (block_idx, b) in blocks.iter().enumerate() {
            let text = b.text();
            let heading = is_heading.get(&(pi, block_idx));
            let figure_idx = in_figure(block_idx);
            let in_drawing = figure_idx.is_some_and(|fi| {
                figure::share_inside(&b.bbox, &figures[fi].drawing) >= FIGURE_CONTAINMENT
            });
            // Only a size tier is a style of its own, whichever evidence
            // found the heading: bold at the body's size is how a chart sets
            // its title too, and the document's other charts, drawn as
            // pictures, set theirs away from any figure. A caption is its
            // figure's, in whatever style it is set, and so is a title set
            // on one line with a caption's label: `FIGURE 17` small, then
            // the chart's title large.
            let larger =
                b.font_size as f64 > body_font as f64 * (1.0 + heading::HEADING_SIZE_MARGIN);
            let documents_heading = !in_drawing
                && larger
                && free_styles.contains(&heading_style(b))
                && !heading::is_caption(&text)
                && !labelled_caption(blocks, block_idx);
            let weak_heading_in_figure = heading.is_some_and(|(_, ev)| {
                heading_yields_to_figure(*ev, figure_idx.is_some(), in_drawing)
                    && !documents_heading
            });
            let figure_of = figure_idx
                .filter(|_| heading.is_none() || weak_heading_in_figure)
                .map(|fi| format!("figure-{pi}-{fi}"));
            let (kind, level, evidence) = match heading {
                Some((l, ev)) if !weak_heading_in_figure => (
                    "doco:SectionTitle",
                    Some(*l),
                    match (ev, form_text.contains(&(pi, block_idx))) {
                        (Evidence::Outline, _) => "outline",
                        (Evidence::Title, _) => "title",
                        (Evidence::Sequence, _) => "sequence",
                        (Evidence::Numbering, false) => "numbering",
                        (Evidence::FontSize, false) => "font-size",
                        (Evidence::Bold, false) => "bold",
                        (Evidence::Numbering, true) => "form-numbering",
                        (Evidence::FontSize, true) => "form-font-size",
                        (Evidence::Bold, true) => "form-bold",
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
                // A line set over the title, smaller than it: the title's
                // kicker, until the title is settled.
                None if kickers.contains(&(pi, block_idx)) => ("doco:Paragraph", None, "kicker"),
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
            let kept = kept_heads.get(pi).is_some_and(|k| k.contains(text.trim()));
            let scrubbed = if retitled || kept {
                text.clone()
            } else if height <= 0.0 {
                furniture::scrub_block(&text, &scrub_texts)
            } else if b.bbox.y0 < band || b.bbox.y1 > height - band {
                furniture::scrub_block(&text, &on_rows(b.bbox.y0 / height, b.bbox.y1 / height))
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
            // A bullet is a list's mark wherever its block ends up: a list
            // item set inside a drawing is the drawing's, and its bullet is
            // no part of what it says.
            let lead = b
                .marker
                .as_deref()
                .and_then(|m| block::strip_run_lead(&scrubbed, m));
            let text = match (block::strip_leading_bullet(&scrubbed), lead) {
                (Some(without), _) if matches!(kind, "doc:ListItem" | "doco:Figure") => {
                    without.to_string()
                }
                // A dash or an arrow that marked a list's item is its marker,
                // as a bullet is.
                (_, Some(without)) if kind == "doc:ListItem" => without.to_string(),
                _ => scrubbed,
            };
            // A count stands in front of its item's text when it is set
            // beside the item's first line (`block::assemble_in_measure`). A
            // count the text doesn't open with stood beside a later line:
            // the block's first line carries on the item before it, and is
            // no item to set under that one.
            let carried_on = b
                .marker
                .as_deref()
                .is_some_and(|m| !block::count_values(m).is_empty() && !text.starts_with(m.trim()));
            // Where the item's marker and text start. A marker set apart
            // stands left of the block's box, which starts at the text; one
            // in the text starts the box.
            item_text.push((kind == "doc:ListItem").then(|| {
                let em = (b.font_size as f64).max(1.0);
                let line = b.lines.first().map_or(b.bbox.x0, |l| l.bbox.x0);
                // A count beside a later line is no mark of this block's.
                let ticked = b.marker.as_deref() == Some(block::CHECKBOX_MARKER);
                match b
                    .marker_x
                    .filter(|&x| !carried_on && x < b.bbox.x0 - 0.3 * em)
                {
                    Some(x) => (x, b.bbox.x0, em, carried_on, ticked),
                    None => (b.bbox.x0, line, em, carried_on, ticked),
                }
            }));
            // Text read from a picture's pixels is OCR's from the start, as
            // `ocr::mark_provenance` would mark it after: the figure doubt
            // below leaves it out.
            let from_picture = !b.lines.is_empty()
                && b.lines
                    .iter()
                    .all(|l| l.ocr == Some(crate::glyph::OcrSource::Region));
            page_elements.push(Element {
                page: pi,
                bbox: Some(b.bbox),
                level,
                figure: figure_of,
                // A heading names its block until elements are numbered,
                // for its level to be settled once the passes below have
                // read some headings as text.
                id: if kind == "doco:SectionTitle" {
                    format!("{HEADING_BLOCK}{pi}:{block_idx}")
                } else {
                    String::new()
                },
                ..Element::new(
                    kind,
                    text,
                    if from_picture { "ocr" } else { "rust" },
                    evidence,
                )
            });
        }
        nest_list_items(&mut page_elements, &item_text);
        coalesce_heading_fragments(&mut page_elements);
        let mut table_elements = Vec::new();
        let layout = &layouts[pi];
        debug_assert_eq!(layout.grid_sources.len(), layout.grids.len());
        // Which grids, read as tables, are in doubt: those anchor for a
        // reading below.
        let mut in_doubt = vec![false; layout.grids.len()];
        for (gi, g) in layout.grids.iter().enumerate() {
            // A running string leaks into a table only where the table
            // reaches its row: a grid takes its glyphs before furniture is
            // read, so a footer crossing it lands in its cells. Elsewhere the
            // string's words are the table's own: a running head parted into
            // its groups names the document again in a note under a table.
            let height = raw.pages[pi].height;
            let scrub_texts = if height > 0.0 {
                on_rows(g.bbox.y0 / height, g.bbox.y1 / height)
            } else {
                scrub_texts.clone()
            };
            // A title band over the table is the table's title, not its
            // header: it is read out as text above the table, and the table
            // is read from the row under it.
            let titled = table_title(
                g,
                &raw.pages[pi],
                &scrub_texts,
                layout.spanning_rows[gi].contains(&0),
            );
            let below_title;
            let (g, shift) = match &titled {
                Some((text, trimmed)) => {
                    let title_box = crate::geom::BBox {
                        x0: g.bbox.x0,
                        y0: g.bbox.y0,
                        x1: g.bbox.x1,
                        y1: trimmed.bbox.y0,
                    };
                    table_elements.push(Element {
                        page: pi,
                        bbox: Some(title_box),
                        ..Element::new("doco:Paragraph", text.clone(), "rust", "table-title")
                    });
                    below_title = trimmed.clone();
                    (&below_title, 1)
                }
                None => (g, 0),
            };
            // A folio the grid took in, rows `r0..r1` holding it: the page's
            // own number where page numbers stand (`furniture::is_folio_cell`),
            // in the grid's row nearest that edge. A table's own figure in a
            // row above its last can match the page's number, as a tax
            // table's line 111 does on page 111. Nor where the page's number
            // is printed outside the grid, its line's top above the grid's or
            // under its foot: the page has its number, and a row's `4` over a
            // foot reading `Page 4 of 6` is the row's.
            let page_h = raw.pages[pi].height.max(1.0);
            let printed_apart = furniture::folio_outside(folio_places[pi], page_h, &g.bbox);
            let unfolio = |t: String, r0: usize, r1: usize| {
                let last = g.ys.len().saturating_sub(1);
                let mid = (g.ys[r0.min(last)] + g.ys[r1.min(last)]) / 2.0 / page_h;
                let edge_row = if mid > 0.5 { r1 >= last } else { r0 == 0 };
                if edge_row
                    && !printed_apart
                    && furniture::is_folio_cell(&t, folios[pi].as_deref(), mid)
                {
                    String::new()
                } else {
                    t
                }
            };
            let cols = g.cols().max(1);
            // A grid set in one of a ruled table's cells is read as a table
            // of its own, after this one; the cell keeps only what it does not
            // take. Only in a table whose rows read as a table's: a frame
            // round a table set in one of its cells is read as before.
            let nested = if layout.grid_sources[gi] == GridSource::Ruled
                && reads_as_rows(g, &raw.pages[pi].glyphs)
            {
                nested_grids(g, &raw.pages[pi])
            } else {
                Vec::new()
            };
            let own_glyphs: Vec<Glyph>;
            let cell_glyphs: &[Glyph] = if nested.is_empty() {
                &raw.pages[pi].glyphs
            } else {
                let taken: Vec<Vec<bool>> = nested
                    .iter()
                    .map(|(n, _, _)| n.takes(&raw.pages[pi].glyphs))
                    .collect();
                own_glyphs = raw.pages[pi]
                    .glyphs
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !taken.iter().any(|t| t[*i]))
                    .map(|(_, gl)| gl.clone())
                    .collect();
                &own_glyphs
            };
            let flat: Vec<String> = match g.layout.as_deref() {
                Some(lay) => lay.texts(&raw.pages[pi].glyphs, g.cols()),
                None => g.cell_texts(cell_glyphs),
            }
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                unfolio(
                    furniture::scrub_cell(&c, &scrub_texts),
                    i / cols,
                    i / cols + 1,
                )
            })
            .collect();
            // A grid inside a drawing is the drawing's labels in rows: a
            // wiring diagram's wires rule its terminal names into a "table",
            // and a chart's ticks and legend line up as one. A grid drawn as
            // a table is one whatever is drawn under it — a watermark behind
            // a catalogue's table, a table's own grey cells: ruled at every
            // row and every column, and at least half of it filled. Wires
            // and dimension lines rule a few of a label grid's boundaries,
            // or rule it everywhere and leave most of it empty.
            let ruled = layout.grid_sources[gi] == GridSource::Ruled;
            // Line art: the strokes inside a grid stand at many more places
            // than its own boundaries, the outlines of a unit's terminals or
            // a drawing's dimension and extension lines. A table is ruled at
            // its boundaries, however many pieces each rule is drawn in.
            let line_art = ruled
                && (line_art(g, &raw.pages[pi].rules)
                    || dimension_drawing(g, &raw.pages[pi], &flat));
            // An axis's scale ruled by its ticks: a row or a column of
            // numbers stepping evenly, `160 140 … 0`, set against a plot.
            let scale = (g.rows() == 1 || g.cols() == 1) && axis_scale(&flat);
            let drawn_as_table = ruled && !line_art && {
                let (occ, vertical, horizontal) =
                    table_confidence_signals(g, &raw.pages[pi].glyphs, &raw.pages[pi].rules);
                occ >= RULED_TABLE_FILL && horizontal > g.rows() && vertical > g.cols()
            };
            // A grid inferred from its text's alignment is kept only where
            // drawn geometry corroborates it: half of it on a drawing, and
            // that geometry is the drawing's, its labels lining up.
            let containment = if layout.grid_sources[gi] == GridSource::Aligned {
                ALIGNED_IN_DRAWING
            } else {
                FIGURE_CONTAINMENT
            };
            let drawn_in = figures
                .iter()
                .position(|f| figure::share_inside(&g.bbox, &f.drawing) >= containment)
                .filter(|_| !drawn_as_table);
            // The grid's text size: what an axis's reach is measured in.
            let line = {
                let mut sizes: Vec<f32> = raw.pages[pi]
                    .glyphs
                    .iter()
                    .filter(|gl| {
                        gl.bbox
                            .is_some_and(|b| figure::share_inside(&b, &g.bbox) >= 0.5)
                    })
                    .map(|gl| gl.font_size)
                    .collect();
                sizes.sort_by(f32::total_cmp);
                sizes.get(sizes.len() / 2).copied().unwrap_or(10.0).max(1.0) as f64
            };
            // The plot a scale stands against: the figure whose drawing is
            // nearest it, within an axis's reach.
            let scaled = if scale && drawn_in.is_none() {
                figures
                    .iter()
                    .enumerate()
                    .map(|(fi, f)| {
                        let d = f.drawing;
                        let gap = (d.x0 - g.bbox.x1)
                            .max(g.bbox.x0 - d.x1)
                            .max(d.y0 - g.bbox.y1)
                            .max(g.bbox.y0 - d.y1)
                            .max(0.0);
                        (fi, gap)
                    })
                    .filter(|(_, gap)| *gap <= line * SCALE_REACH)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(fi, _)| fi)
            } else {
                None
            };
            // Charts set side by side, their scales lined up into a grid.
            let paneled = if drawn_in.is_none() && !scale && g.cols() >= 2 {
                panel_axes(g, &flat, figures, line)
            } else {
                None
            };
            let drawn_in = drawn_in.or(scaled).or(paneled);
            if layout.demoted_tables[gi] || drawn_in.is_some() || line_art || scale {
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
                table_elements.push(match drawn_in {
                    Some(fi) => Element {
                        page: pi,
                        bbox: Some(g.bbox),
                        figure: Some(format!("figure-{pi}-{fi}")),
                        ..Element::new("doco:Figure", text, "rust", "fills")
                    },
                    None if line_art || scale => Element {
                        page: pi,
                        bbox: Some(g.bbox),
                        ..Element::new("doco:Figure", text, "rust", "fills")
                    },
                    None => Element {
                        page: pi,
                        bbox: Some(g.bbox),
                        ..Element::new("doco:Paragraph", text, "rust", "layout-demoted")
                    },
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
            for r in layout.spanning_rows[gi]
                .iter()
                .filter_map(|&r| r.checked_sub(shift))
            {
                rows[r][0] = unfolio(
                    furniture::scrub_cell(&g.row_text(&raw.pages[pi].glyphs, r), &scrub_texts),
                    r,
                    r + 1,
                );
                for (c, cell) in rows[r].iter_mut().enumerate().skip(1) {
                    cell.clear();
                    merges.continues_left[r * cols + c] = true;
                }
            }
            // A label's dot leaders run it up to the next column: they are
            // the gutter's, not the cell's text, however the table was read.
            let min = match explicit {
                Some(_) => 3,
                None => crate::textgrid::LEADER_DOTS,
            };
            for cell in rows.iter_mut().flatten() {
                *cell = crate::textgrid::strip_leaders_of(cell, min);
            }
            let mut header_rows = match explicit {
                Some(lay) => lay.header_rows,
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
                && names_spanned_columns(&rows, &merges.continues_left, &merges.continues_above)
            {
                header_rows = 2;
            }
            // Under a title read out above the table, the row the title
            // leaves on top must earn the header.
            if titled.is_some() {
                let page = &raw.pages[pi];
                if !g.heads_its_rows(header_rows, &rows, &page.glyphs, &page.rules, &page.fills) {
                    header_rows = 0;
                }
            }
            // Under a title or not, a grid of figures headed by its parameters' rows: the IES
            // coefficients of utilization under `RC %: | 80` and `RW %: | 70
            // | 50 | 30 | 10`, each row a parameter's name and its values,
            // over rows keyed by another (`RCR: 0`, `1`, `2`, …). Read as
            // data, no value was named by the reflectances it is for.
            if header_rows == 0 {
                header_rows = parameter_rows(&rows);
            }
            // A merged cell is read whole, into its top-left cell. Its words
            // were dealt out by the grid's cuts through it, which split a
            // centred header across the row boundary under it (`time` above,
            // `Delivery` below) and a band across the columns it spans
            // (`Crucible | Logistics`).
            let rcount = rows.len();
            let mut taken = vec![false; rcount * cols];
            // Cells read row by row out of a block that could not be read
            // whole: they no longer carry on the cell above.
            let mut unspanned: Vec<usize> = Vec::new();
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
                            Some(t) => unfolio(furniture::scrub_cell(&t, &scrub_texts), r, r_end),
                            None if held.len() == 1 => rows[held[0].0][held[0].1].clone(),
                            None => {
                                // A block across rows and columns that reads
                                // whole neither way is two spans crossing: a
                                // group label down its column, and each row's
                                // label across the columns beside it. Each
                                // row is read across the columns its own
                                // ruling leaves open, past the block's edge
                                // where a rule parting the block's top row
                                // stops short of it: `Self-diagnostic
                                // function` under a sub-column `Memory
                                // backup` rules.
                                if r_end - r > 1 && c_end - c > 1 {
                                    let glyphs = &raw.pages[pi].glyphs;
                                    // Where the top row sets the group label
                                    // and a value of its own beside it, apart,
                                    // a row below with nothing under the label
                                    // and its text in one cell beside it sets
                                    // that cell's value, and it stays there:
                                    // `Smoke developed` under `Surface-burning
                                    // characteristics`, beside `Flame spread`.
                                    let filled = |row: &[String]| {
                                        (c..c_end)
                                            .filter(|&k| !row[k].trim().is_empty())
                                            .collect::<Vec<_>>()
                                    };
                                    let top = filled(&rows[r]);
                                    let split = top.len() >= 2
                                        && top[0] == c
                                        && g.region_text(glyphs, &merges, r..r + 1, c..c_end)
                                            .is_none();
                                    for (i, row) in rows.iter_mut().enumerate().take(r_end).skip(r)
                                    {
                                        if split
                                            && i > r
                                            && matches!(filled(row).as_slice(), [k] if *k > c)
                                        {
                                            continue;
                                        }
                                        let open = |k: usize| {
                                            merges.continues_left[i * cols + k]
                                                && (k < c_end
                                                    || (!taken[i * cols + k]
                                                        && !merges.continues_above[i * cols + k]))
                                        };
                                        let mut ce = c + 1;
                                        while ce < cols && open(ce) {
                                            ce += 1;
                                        }
                                        if ce - c < 2 {
                                            continue;
                                        }
                                        let read = |ce: usize| {
                                            g.region_text(glyphs, &merges, i..i + 1, c..ce)
                                                .map(|t| (t, ce))
                                        };
                                        let inside = ce.min(c_end);
                                        let Some((t, ce)) = read(ce).or_else(|| {
                                            (ce > inside && inside - c >= 2)
                                                .then(|| read(inside))
                                                .flatten()
                                        }) else {
                                            continue;
                                        };
                                        row[c..ce].iter_mut().for_each(String::clear);
                                        row[c] = unfolio(
                                            furniture::scrub_cell(&t, &scrub_texts),
                                            i,
                                            i + 1,
                                        );
                                        (c..ce).for_each(|k| taken[i * cols + k] = true);
                                        unspanned.extend((c..ce).map(|k| i * cols + k));
                                    }
                                }
                                // A cell left holding text of its own beside
                                // another's is a value, not the rest of the
                                // cell to its left, unless the two read as one
                                // across the cut: `Flame spread` beside its
                                // group label, `Apt. no.` beside an address
                                // label the cut runs through.
                                // Cells that do read as one are one, their
                                // text read whole into the first: a value set
                                // across two model columns beside its row's
                                // label (`11/12 seconds (ventilated); 10/12
                                // seconds (refrigerated)`).
                                let glyphs = &raw.pages[pi].glyphs;
                                let homes = g.glyph_cells(glyphs);
                                // A line runs on across the cut at `k` in row
                                // `i`: a word left of it and the next on its
                                // baseline a word space apart. Values in
                                // columns set close stand further apart.
                                let runs_across = |i: usize, p: usize, k: usize| {
                                    let at = |cell: usize| {
                                        glyphs.iter().zip(&homes).filter(move |(gl, h)| {
                                            **h == Some(cell)
                                                && gl.bbox.is_some()
                                                && !gl.text.trim().is_empty()
                                        })
                                    };
                                    at(i * cols + k).any(|(b, _)| {
                                        let (bb, fs) =
                                            (b.bbox.unwrap(), b.font_size.max(1.0) as f64);
                                        (p..k).any(|q| {
                                            at(i * cols + q).any(|(a, _)| {
                                                let ab = a.bbox.unwrap();
                                                (a.origin.1 - b.origin.1).abs() < 0.3 * fs
                                                    && ab.x1 <= bb.x0 + 0.5
                                                    && bb.x0 - ab.x1 <= 0.5 * fs
                                            })
                                        })
                                    })
                                };
                                let mut apart: Vec<usize> = Vec::new();
                                for i in r..r_end {
                                    let mut start: Option<usize> = None;
                                    for k in c..c_end {
                                        if rows[i][k].trim().is_empty() {
                                            continue;
                                        }
                                        let joined = start.and_then(|p| {
                                            g.region_text(glyphs, &merges, i..i + 1, p..k + 1)
                                                .map(|t| (p, t))
                                        });
                                        // Cells read as one keep the span; their
                                        // text is joined where a line runs across.
                                        if let Some((p, _)) = &joined {
                                            if !runs_across(i, *p, k) {
                                                start = Some(k);
                                                continue;
                                            }
                                        }
                                        match joined {
                                            Some((p, t)) => {
                                                rows[i][p] = unfolio(
                                                    furniture::scrub_cell(&t, &scrub_texts),
                                                    i,
                                                    i + 1,
                                                );
                                                rows[i][k].clear();
                                            }
                                            None => {
                                                if k > c {
                                                    apart.push(i * cols + k);
                                                }
                                                start = Some(k);
                                            }
                                        }
                                    }
                                }
                                for k in apart {
                                    merges.continues_left[k] = false;
                                }
                                continue;
                            }
                        };
                    for row in rows.iter_mut().take(r_end).skip(r) {
                        row[c..c_end].iter_mut().for_each(String::clear);
                    }
                    rows[r][c] = text;
                }
            }
            for k in unspanned {
                merges.continues_above[k] = false;
            }
            // Banner bands below the header block are sub-headers: they label
            // the rows beneath them rather than the columns.
            let sub_headers: Vec<usize> = match explicit {
                Some(lay) => lay.sub_headers.clone(),
                // Not a row the ruling leaves open but whose values stand
                // apart in their own cells: a table ruled with horizontals
                // only leaves every row open.
                None => merges
                    .full_width_row
                    .iter()
                    .enumerate()
                    .filter(|(r, full)| {
                        let spans = (1..cols).all(|c| merges.continues_left[*r * cols + c]);
                        (**full && spans && rows[*r].iter().any(|c| !c.trim().is_empty())
                            || group_label(&rows, *r))
                            && *r >= header_rows
                    })
                    .map(|(r, _)| r)
                    .collect(),
            };
            let restated_headers = match explicit {
                Some(lay) => lay.restated_headers.clone(),
                None => {
                    let mut restated = g.restated_header_rows(
                        &rows,
                        header_rows,
                        &sub_headers,
                        &raw.pages[pi].glyphs,
                        &raw.pages[pi].fills,
                    );
                    restated.extend(
                        nested_header_rows(&rows, &merges.continues_above, header_rows)
                            .into_iter()
                            .filter(|r| !sub_headers.contains(r)),
                    );
                    restated.sort_unstable();
                    restated.dedup();
                    restated
                }
            };
            let spread = spread_runs(g, &raw.pages[pi].glyphs, &rows, &merges.continues_left);
            tables += 1;
            let text = rows
                .iter()
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("\n");
            table_elements.push(Element {
                page: pi,
                bbox: Some(g.bbox),
                spread_left: spread.iter().any(|x| *x).then_some(spread),
                cells: Some(rows),
                header_rows: Some(header_rows),
                sub_headers: (!sub_headers.is_empty()).then_some(sub_headers),
                restated_headers: (!restated_headers.is_empty()).then_some(restated_headers),
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
            let this_table = table_elements.len() - 1;
            in_doubt[gi] = table_elements.last().is_some_and(|t| {
                table_in_doubt(
                    t,
                    suspect_tables
                        .iter()
                        .any(|s| s.page == pi && s.bbox.intersects(&g.bbox)),
                )
            });
            frames.push(TableFrame {
                page: pi,
                bbox: g.bbox,
                xs: g.xs.clone(),
                styled: g.opens_styled(&raw.pages[pi].glyphs, &raw.pages[pi].fills),
                bold: g.opens_bold(&raw.pages[pi].glyphs),
            });
            for (n, r, c) in &nested {
                let label = (*c > 0)
                    .then(|| table_elements.last())
                    .flatten()
                    .and_then(|e: &Element| e.cells.as_ref())
                    .and_then(|rows| rows.get(r.saturating_sub(shift)))
                    .and_then(|row| row.first())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                table_elements.push(nested_table(n, &raw.pages[pi], pi, label, &scrub_texts));
                tables += 1;
            }
            drop_blank_rows(&mut table_elements[this_table]);
        }
        // Table-confidence anchors: a table whose reading is in doubt
        // ([`table_in_doubt`]) anchors for a model's reading of its crop. The
        // anchor sits immediately above its table so the hybrid adapter can
        // replace the pipe rows with the VLM's reading of the same region;
        // the pure engine's output is untouched. A grid read as no table
        // (demoted, or a drawing's labels) has none to replace and does not
        // anchor.
        if opts.emit_anchors {
            for (ti, g) in layout.grids.iter().enumerate() {
                if layout.demoted_tables[ti] || !in_doubt[ti] {
                    continue;
                }
                // A document with this many drawn grids is a chart or a
                // ledger built from one template, and its ruling is the
                // template's: a reading per table would cost a reading per
                // page of an 800-page record to confirm what the rules
                // already say. Escalation is earned per table by a header
                // in doubt or a disagreeing structure, not by count.
                if layout.grid_sources[ti] == GridSource::Ruled
                    && ruled_tables > TRUSTED_RULING_TABLES
                {
                    continue;
                }
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
                    // A region OCR read is text now; its number stays taken.
                    if raw.pages[pi].ocr_read_region(ri) {
                        continue;
                    }
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
            let id = format!("elem-{n:05}");
            if let Some(block) = element
                .id
                .strip_prefix(HEADING_BLOCK)
                .and_then(|r| r.split_once(':'))
                .and_then(|(p, b)| Some((p.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
            {
                heading_block.insert(id.clone(), block);
            }
            element.id = id;
            elements.push(element);
        }
    }
    link_continued_tables(&mut elements, &frames);
    fold_header_boxes(&mut elements);
    drop_merged_columns(&mut elements);
    demote_empty_bold_heads(&mut elements);
    read_form_text(&mut elements);
    read_blank_fields(&mut elements);
    read_table_heads(&mut elements);
    read_entry_lists(&mut elements);
    read_contents_rows(&mut elements);
    read_continued_heads(&mut elements);
    // The levels of the headings that stand, each against those open above
    // it: a pass above may have read a heading as text.
    {
        let at: Vec<usize> = (0..elements.len())
            .filter(|&i| elements[i].kind == "doco:SectionTitle")
            .collect();
        let hs: Vec<(Option<&heading::LevelInfo>, usize)> = at
            .iter()
            .map(|&i| {
                let e = &elements[i];
                (
                    heading_block.get(&e.id).and_then(|k| level_info.get(k)),
                    e.level.unwrap_or(1),
                )
            })
            .collect();
        let levels = heading::settle_levels(&hs);
        for (i, level) in at.into_iter().zip(levels) {
            elements[i].level = Some(level);
        }
    }
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

    // A symbol the font mapped to a control character is no character a
    // reader can show: a list's mark set as 0x17 has been read as one by
    // now, and what is left of any other goes.
    for e in &mut elements {
        e.scrub_controls();
        // A title broken over lines is one title: a heading's lines, set
        // one under another by hand, read as one run.
        if e.kind == "doco:SectionTitle" && e.text.contains('\n') {
            e.text = e.text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    settle_title(
        &mut elements,
        raw.title.as_deref(),
        outline,
        |e| {
            heading_block
                .get(&e.id)
                .map(|&(p, b)| &blocks_per_page[p][b])
        },
        body_font,
    );
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
    // A raised figure after a word is a note's number, no letter of the
    // word: `consoli-` over `dated²` is `consolidated`.
    let is_word_char = |c: char| {
        (c.is_alphanumeric() && fluree_doc_model::marks::raised_figure(c).is_none())
            || c == '-'
            || c == brk
    };
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

    /// A word OCR read in a picture: one glyph at `(x0, y0)`, 5pt a
    /// character, 10pt tall.
    fn read_word(x0: f64, y0: f64, text: &str) -> Glyph {
        let w = 5.0 * text.chars().count() as f64;
        Glyph {
            text: text.into(),
            bbox: Some(BBox {
                x0,
                y0,
                x1: x0 + w,
                y1: y0 + 10.0,
            }),
            page: 0,
            origin: (x0, y0 + 8.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: Some(w),
            draw_index: 0,
            ocr: Some(crate::glyph::OcrSource::Region),
        }
    }

    fn across(y: f64, x0: f64, x1: f64) -> crate::rule::Rule {
        crate::rule::Rule {
            bbox: BBox {
                x0,
                y0: y,
                x1,
                y1: y + 0.5,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        }
    }

    fn down(x: f64, y0: f64, y1: f64) -> crate::rule::Rule {
        crate::rule::Rule {
            bbox: BBox {
                x0: x,
                y0,
                x1: x + 0.5,
                y1,
            },
            orientation: crate::rule::Orientation::Vertical,
            page: 0,
        }
    }

    #[test]
    fn a_picture_ruled_as_a_table_is_read_as_one() {
        // Three columns, a header and three rows, every cell ruled; one row
        // leaves a cell empty.
        let mut glyphs = Vec::new();
        for (r, row) in [
            ["Tube", "Buffer", "DNA"],
            ["S1", "3 uL", "10 uL"],
            ["S2", "3 uL", ""],
            ["EB", "3 uL", "10 uL"],
        ]
        .iter()
        .enumerate()
        {
            for (c, text) in row.iter().enumerate() {
                if !text.is_empty() {
                    glyphs.push(read_word(
                        10.0 + 100.0 * c as f64,
                        5.0 + 20.0 * r as f64,
                        text,
                    ));
                }
            }
        }
        let mut rules: Vec<_> = (0..5)
            .map(|r| across(20.0 * r as f64, 0.0, 300.0))
            .collect();
        rules.extend((0..4).map(|c| down(100.0 * c as f64, 0.0, 80.0)));
        assert!(picture_table(&glyphs, &rules, 0));
        // Without its rules, or with a frame round it alone, it is no table
        // here: alignment is not read from a picture.
        assert!(!picture_table(&glyphs, &[], 0));
        let frame = [
            across(0.0, 0.0, 300.0),
            across(80.0, 0.0, 300.0),
            down(0.0, 0.0, 80.0),
            down(300.0, 0.0, 80.0),
        ];
        assert!(!picture_table(&glyphs, &frame, 0));
    }

    #[test]
    fn underlines_and_strike_through_in_a_framed_picture_make_no_table() {
        // A framed panel of prose: a heading underlined, a word struck
        // through, a link underlined, and the frame.
        let glyphs = vec![
            read_word(10.0, 5.0, "Before you scan"),
            read_word(10.0, 25.0, "Load the document face down"),
            read_word(10.0, 45.0, "Close the cover"),
            read_word(10.0, 65.0, "See the online guide"),
        ];
        let rules = vec![
            across(0.0, 0.0, 200.0),
            across(100.0, 0.0, 200.0),
            down(0.0, 0.0, 100.0),
            down(200.0, 0.0, 100.0),
            // The heading's underline, the struck word, the link's underline.
            across(16.0, 10.0, 85.0),
            across(50.0, 10.0, 85.0),
            across(76.0, 10.0, 110.0),
        ];
        assert!(!picture_table(&glyphs, &rules, 0));
    }

    #[test]
    fn a_diagram_s_boxes_are_no_table() {
        // Three boxes in a row, each round a label, joined by arrows whose
        // labels, and a legend and a title, stand outside them.
        let mut glyphs = vec![
            read_word(10.0, 0.0, "Collateral management system architecture"),
            read_word(10.0, 40.0, "Interfaces"),
            read_word(160.0, 40.0, "Reference data"),
            read_word(330.0, 40.0, "Pool"),
            read_word(100.0, 25.0, "updates"),
            read_word(260.0, 25.0, "positions"),
            read_word(100.0, 75.0, "settlement"),
            read_word(260.0, 75.0, "instructions"),
            read_word(10.0, 95.0, "Source: ECMS documentation and annual review"),
        ];
        glyphs.push(read_word(
            10.0,
            110.0,
            "Legend: arrows show the flow of messages",
        ));
        let mut rules = Vec::new();
        for x0 in [0.0, 150.0, 320.0] {
            rules.push(across(35.0, x0, x0 + 90.0));
            rules.push(across(55.0, x0, x0 + 90.0));
            rules.push(down(x0, 35.0, 55.0));
            rules.push(down(x0 + 90.0, 35.0, 55.0));
        }
        assert!(!picture_table(&glyphs, &rules, 0));
    }

    #[test]
    fn a_screen_s_panel_or_list_of_buttons_is_no_table() {
        // A language list framed with its scroll bar, over its OK button:
        // every edge drawn, but one block of text a row.
        let glyphs = vec![
            read_word(10.0, 5.0, "English"),
            read_word(10.0, 20.0, "Deutsch"),
            read_word(10.0, 35.0, "Suomi"),
            read_word(10.0, 50.0, "Svenska"),
            read_word(170.0, 75.0, "OK"),
        ];
        let rules = vec![
            across(0.0, 0.0, 200.0),
            across(70.0, 0.0, 200.0),
            across(90.0, 0.0, 200.0),
            down(0.0, 0.0, 90.0),
            down(180.0, 0.0, 70.0),
            down(200.0, 0.0, 90.0),
            down(160.0, 70.0, 90.0),
        ];
        assert!(!picture_table(&glyphs, &rules, 0));
        // A menu of buttons, an icon's frame beside each label: the boxes
        // line up, the space between them undrawn.
        let mut glyphs = Vec::new();
        let mut rules = Vec::new();
        for (r, label) in [
            "Home Page",
            "Solutions Center",
            "Supplies",
            "Creative Center",
        ]
        .iter()
        .enumerate()
        {
            let y = 30.0 * r as f64;
            glyphs.push(read_word(40.0, y + 6.0, "@"));
            glyphs.push(read_word(70.0, y + 6.0, label));
            rules.push(across(y, 30.0, 200.0));
            rules.push(across(y + 22.0, 30.0, 200.0));
            rules.push(down(30.0, y, y + 22.0));
            rules.push(down(60.0, y, y + 22.0));
            rules.push(down(200.0, y, y + 22.0));
        }
        assert!(!picture_table(&glyphs, &rules, 0));
    }

    #[test]
    fn a_box_round_part_of_a_picture_is_no_table() {
        // A panel ruled as a table inside a larger picture whose text runs
        // beyond it: the ruling does not run across the picture's text.
        let mut glyphs = Vec::new();
        for r in 0..3 {
            glyphs.push(read_word(10.0, 5.0 + 20.0 * r as f64, "Size"));
            glyphs.push(read_word(110.0, 5.0 + 20.0 * r as f64, "Large"));
        }
        for r in 0..8 {
            glyphs.push(read_word(250.0, 5.0 + 20.0 * r as f64, "Settings"));
        }
        let mut rules: Vec<_> = (0..4)
            .map(|r| across(20.0 * r as f64, 0.0, 200.0))
            .collect();
        rules.extend((0..3).map(|c| down(100.0 * c as f64, 0.0, 60.0)));
        assert!(!picture_table(&glyphs, &rules, 0));
    }

    fn line_at(
        kind: &str,
        text: &str,
        evidence: &'static str,
        level: Option<usize>,
        y: f64,
        x: f64,
    ) -> Element {
        Element {
            level,
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 80.0,
                y1: y + 10.0,
            }),
            ..Element::new(kind, text, "rust", evidence)
        }
    }

    #[test]
    fn a_list_item_set_under_the_text_of_the_one_before_nests_in_it() {
        // 10pt items: (marker x, text x, y), each 10 high, 4 apart.
        let rows: &[(&str, f64, f64, f64)] = &[
            ("Before you start:", 50.0, 62.0, 100.0),
            // Its marker under the text above: one deeper, then two.
            ("Turn off the power", 63.0, 74.0, 114.0),
            ("Check the breaker", 64.0, 75.0, 128.0),
            ("At the panel", 76.0, 86.0, 142.0),
            // Back out to the first level's marker.
            ("Mount the bracket", 50.0, 62.0, 156.0),
            // Half an em right: the same list's marker drawn off.
            ("Fit the cover", 55.0, 66.0, 170.0),
            // Far past the text above: another column's list, not a sub-list.
            ("Order parts", 120.0, 131.0, 184.0),
        ];
        let mut elements: Vec<Element> = rows
            .iter()
            .map(|&(t, x, _, y)| line_at("doc:ListItem", t, "marker", None, y, x))
            .collect();
        let mut text: Vec<Option<ListMark>> = rows
            .iter()
            .map(|r| Some((r.1, r.2, 10.0, false, false)))
            .collect();
        // After a paragraph, and in the next column (back up the page), a
        // list starts again at the top.
        elements.push(line_at(
            "doco:Paragraph",
            "Note",
            "layout",
            None,
            200.0,
            50.0,
        ));
        text.push(None);
        elements.push(line_at(
            "doc:ListItem",
            "Wire it",
            "marker",
            None,
            214.0,
            63.0,
        ));
        text.push(Some((63.0, 74.0, 10.0, false, false)));
        elements.push(line_at(
            "doc:ListItem",
            "Test it",
            "marker",
            None,
            100.0,
            313.0,
        ));
        text.push(Some((313.0, 324.0, 10.0, false, false)));
        nest_list_items(&mut elements, &text);
        let depths: Vec<usize> = elements.iter().map(|e| e.depth).collect();
        assert_eq!(depths, [0, 1, 1, 2, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_list_items_marker_stands_where_it_is_drawn() {
        let item =
            |t: &str, y: f64, box_x: f64| line_at("doc:ListItem", t, "marker", None, y, box_x);
        // One list, its first bullet in the line (the box starts at it) and
        // the next drawn apart at the same place (the box starts at the
        // text): siblings. Then a sub-item whose drawn bullet stands under
        // the text: one deeper.
        let mut elements = vec![
            item("Plan the route", 100.0, 40.0),
            item("Book the rooms", 114.0, 52.0),
            item("Two nights", 128.0, 64.0),
        ];
        let text = vec![
            Some((40.0, 40.0, 10.0, false, false)),
            Some((40.0, 52.0, 10.0, false, false)),
            Some((52.0, 64.0, 10.0, false, false)),
        ];
        nest_list_items(&mut elements, &text);
        let depths: Vec<usize> = elements.iter().map(|e| e.depth).collect();
        assert_eq!(depths, [0, 0, 1]);
        // A box to tick marks a choice: what is set under it stays level.
        let mut elements = vec![
            item("Yes", 100.0, 52.0),
            item("Describe below", 114.0, 70.0),
        ];
        let text = vec![
            Some((40.0, 52.0, 10.0, false, true)),
            Some((58.0, 70.0, 10.0, false, false)),
        ];
        nest_list_items(&mut elements, &text);
        assert_eq!(elements[1].depth, 0);
    }

    #[test]
    fn a_list_or_contents_set_as_headings_is_text() {
        let head =
            |t: &str, y: f64, x: f64| line_at("doco:SectionTitle", t, "font-size", Some(4), y, x);
        let para = |t: &str, y: f64, x: f64| line_at("doco:Paragraph", t, "layout", None, y, x);
        // A list of links, each over its second label.
        let mut els = vec![
            head("North", 100.0, 40.0),
            para("First Region", 112.0, 40.0),
            head("South", 130.0, 40.0),
            para("Second Region", 142.0, 40.0),
            head("East", 160.0, 40.0),
            para("Third Region", 172.0, 40.0),
            head("Background", 200.0, 40.0),
            para(
                "A paragraph that heads its section, as text does over several words.",
                212.0,
                40.0,
            ),
        ];
        read_entry_lists(&mut els);
        let kinds: Vec<&str> = els.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(&kinds[..6], ["doco:Paragraph"; 6]);
        assert_eq!(kinds[6], "doco:SectionTitle");
        // A contents' later page: entries with their pages, in order.
        let mut els = vec![
            head("PART II", 100.0, 40.0),
            para("Planning", 100.0, 120.0),
            para("12", 100.0, 300.0),
            head("PART III", 120.0, 40.0),
            para("Design", 120.0, 120.0),
            para("19", 120.0, 300.0),
            head("PART IV", 140.0, 40.0),
            para("Delivery", 140.0, 120.0),
            para("27", 140.0, 300.0),
        ];
        read_contents_rows(&mut els);
        assert!(els.iter().all(|e| e.kind != "doco:SectionTitle"));
        // A catalogue's quantities end its rows too, but repeat and fall.
        let mut els = vec![
            head("Seal kit", 100.0, 40.0),
            para("4", 100.0, 300.0),
            head("Pump set", 120.0, 40.0),
            para("1", 120.0, 300.0),
            head("Gasket", 140.0, 40.0),
            para("1", 140.0, 300.0),
        ];
        read_contents_rows(&mut els);
        assert!(els.iter().filter(|e| e.kind == "doco:SectionTitle").count() == 3);
    }

    #[test]
    fn a_tables_column_head_left_above_it_is_no_heading() {
        let table = |y0: f64| {
            let mut t = line_at("doco:Table", "Rows | 2031 | 2030", "layout", None, y0, 60.0);
            t.bbox = Some(BBox {
                x0: 60.0,
                y0,
                x1: 540.0,
                y1: y0 + 80.0,
            });
            t
        };
        let head = |text: &str, x0: f64, x1: f64, y0: f64| {
            let mut e = line_at("doco:SectionTitle", text, "bold", Some(3), y0, x0);
            e.bbox = Some(BBox {
                x0,
                y0,
                x1,
                y1: y0 + 6.0,
            });
            e
        };
        let mut elements = vec![
            // A group's head over the two value columns at the right.
            head("Total Holdings", 440.0, 500.0, 100.0),
            table(110.0),
            // A section's heading over a table, from its left edge.
            head("Holdings by Type", 60.0, 160.0, 300.0),
            table(310.0),
            // A table's title centred over it.
            head("Summary of Holdings", 250.0, 350.0, 500.0),
            table(510.0),
        ];
        read_table_heads(&mut elements);
        assert_eq!(
            heads(&elements),
            ["Holdings by Type", "Summary of Holdings"]
        );
    }

    #[test]
    fn a_label_over_a_blank_box_is_no_heading() {
        // A sheet's footer: two labels side by side over empty boxes for a
        // stamp, and a lead-in over its table and another over its list.
        let mut elements = vec![
            line_at(
                "doco:SectionTitle",
                "SIGNED BY:",
                "bold",
                Some(3),
                700.0,
                300.0,
            ),
            line_at("doco:SectionTitle", "STAMP:", "bold", Some(3), 700.0, 420.0),
            line_at("doco:SectionTitle", "Sizes:", "bold", Some(3), 400.0, 40.0),
            line_at("doco:Table", "S | M | L", "layout", None, 412.0, 40.0),
            line_at(
                "doco:SectionTitle",
                "Includes:",
                "bold",
                Some(3),
                500.0,
                40.0,
            ),
            line_at("doco:ListItem", "Two brackets", "layout", None, 511.0, 50.0),
            line_at(
                "doco:Paragraph",
                "Printed 2030",
                "layout",
                None,
                760.0,
                60.0,
            ),
        ];
        // An article's label beside its text, the paragraph's first line
        // running on from it and wrapping under it; a short lead-in over
        // items set in past its end.
        let mut article = line_at(
            "doco:Paragraph",
            "The shares are …",
            "layout",
            None,
            299.0,
            40.0,
        );
        article.bbox = Some(BBox {
            x0: 40.0,
            y0: 299.0,
            x1: 540.0,
            y1: 350.0,
        });
        elements.push(line_at(
            "doco:SectionTitle",
            "SIXTH:",
            "bold",
            Some(3),
            300.0,
            100.0,
        ));
        elements.push(article);
        let mut lead = line_at("doco:SectionTitle", "Talks:", "bold", Some(3), 600.0, 40.0);
        lead.bbox = Some(BBox {
            x0: 40.0,
            y0: 600.0,
            x1: 70.0,
            y1: 607.0,
        });
        elements.push(lead);
        elements.push(line_at(
            "doc:ListItem",
            "A panel on records",
            "layout",
            None,
            618.0,
            76.0,
        ));
        read_blank_fields(&mut elements);
        assert_eq!(
            heads(&elements),
            ["Sizes:", "Includes:", "SIXTH:", "Talks:"]
        );
    }

    fn heads(elements: &[Element]) -> Vec<&str> {
        elements
            .iter()
            .filter(|e| e.kind == "doco:SectionTitle")
            .map(|e| e.text.as_str())
            .collect()
    }

    #[test]
    fn a_dimension_lines_arrowheads_sit_on_its_ends() {
        use crate::rule::{Fill, Orientation, Rule};
        // A vertical dimension line drawn to its arrows' bases, its callouts
        // set at 6.5 pt.
        let line = Rule {
            bbox: BBox {
                x0: 209.7,
                y0: 320.0,
                x1: 210.3,
                y1: 410.0,
            },
            orientation: Orientation::Vertical,
            page: 0,
        };
        let head = |x0, y0, x1, y1| Fill {
            bbox: BBox { x0, y0, x1, y1 },
            page: 0,
            paint: None,
            stroked: false,
        };
        assert!(ends_in_arrowhead(
            &line,
            &[head(208.2, 315.5, 211.8, 321.0)],
            6.5
        ));
        assert!(ends_in_arrowhead(
            &line,
            &[head(208.1, 409.0, 211.9, 414.6)],
            6.5
        ));
        // Wider than long, halfway along the line, or a box the line runs
        // into: no arrowhead.
        assert!(!ends_in_arrowhead(
            &line,
            &[head(205.0, 316.0, 215.0, 321.0)],
            6.5
        ));
        assert!(!ends_in_arrowhead(
            &line,
            &[head(208.2, 360.0, 211.8, 366.0)],
            6.5
        ));
        assert!(!ends_in_arrowhead(
            &line,
            &[head(190.0, 300.0, 230.0, 330.0)],
            6.5
        ));
    }

    #[test]
    fn callouts_are_figures_units_and_counts() {
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(callouts(&c(&[
            "31 1/4\"", "(794 mm)", "", "4X", "12.5 TYP", "B"
        ])));
        // A label names a row: a table's. Letters alone measure nothing.
        assert!(!callouts(&c(&["Width", "31 1/4\""])));
        assert!(!callouts(&c(&["", "A", "B"])));
    }

    #[test]
    fn charts_side_by_side_read_their_scales_as_axes() {
        // Two panels, each a title over a scale from 60 down to 0, the
        // scales lined up into a two-column grid; each panel's plot stands
        // just right of its scale.
        let grid = Grid {
            page: 0,
            xs: vec![70.0, 96.0, 330.0, 356.0],
            ys: vec![100.0, 112.0, 140.0, 168.0, 196.0, 224.0],
            bbox: BBox {
                x0: 70.0,
                y0: 100.0,
                x1: 356.0,
                y1: 224.0,
            },
            layout: None,
        };
        let row = |a: &str, b: &str| vec![a.to_string(), String::new(), b.to_string()];
        let mut cells = row("31 Copper", "48 Zinc");
        for v in ["60", "40", "20", "0"] {
            cells.extend(row(v, v));
        }
        let plot = |x0: f64| figure::Figure {
            bbox: BBox {
                x0,
                y0: 114.0,
                x1: x0 + 190.0,
                y1: 230.0,
            },
            drawing: BBox {
                x0,
                y0: 114.0,
                x1: x0 + 190.0,
                y1: 230.0,
            },
            page: 0,
            shapes: 24,
            frame: None,
            axes: false,
        };
        let plots = [plot(100.0), plot(360.0)];
        assert_eq!(panel_axes(&grid, &cells, &plots, 7.0), Some(0));
        // With no chart beside them, the columns are a table's.
        assert_eq!(panel_axes(&grid, &cells, &[], 7.0), None);
        // Nor where a column's figures step unevenly: data.
        let mut data = row("31 Copper", "48 Zinc");
        for (a, b) in [("60", "12"), ("40", "7"), ("20", "31"), ("0", "4")] {
            data.extend(row(a, b));
        }
        assert_eq!(panel_axes(&grid, &data, &plots, 7.0), None);
        // Nor where the charts stand far off, past an axis's reach.
        let far = [plot(160.0), plot(420.0)];
        assert_eq!(panel_axes(&grid, &cells, &far, 7.0), None);
    }

    #[test]
    fn numbers_stepping_evenly_are_a_scale() {
        let cells = |c: &[&str]| c.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        // By one difference, falling or rising, signed or with thousands.
        assert!(axis_scale(&cells(&["90", "75", "60", "45", "30"])));
        assert!(axis_scale(&cells(&["\u{2212}0.5", "0", "0.5", "1.0"])));
        assert!(axis_scale(&cells(&["2,000 4,000 6,000"])));
        // By one ratio, as a logarithmic axis steps.
        assert!(axis_scale(&cells(&["0.1", "1", "10", "100"])));
        // A column of counts, two numbers, a label, or no step at all.
        assert!(!axis_scale(&cells(&["12", "7", "31", "4"])));
        assert!(!axis_scale(&cells(&["20", "40"])));
        assert!(!axis_scale(&cells(&["Total", "10", "20", "30"])));
        assert!(!axis_scale(&cells(&["5", "5", "5"])));
    }

    #[test]
    fn a_reading_ruled_through_the_gutter_is_one_table() {
        // A reading x 50-550 over four rows from y 100 to 180; the gutter
        // found in it at x 300.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![50.0, 175.0, 300.0, 425.0, 550.0],
            ys: vec![100.0, 120.0, 140.0, 160.0, 180.0],
            bbox: BBox {
                x0: 50.0,
                y0: 100.0,
                x1: 550.0,
                y1: 180.0,
            },
        };
        let h = |x0: f64, x1: f64, y: f64| crate::rule::Rule {
            bbox: BBox {
                x0,
                y0: y - 0.25,
                x1,
                y1: y + 0.25,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        // Prose under the reading from y 200, the page's foot at y 192.
        let text = |x: f64, y: f64| Glyph {
            text: "a".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y,
                x1: x + 5.0,
                y1: y + 8.0,
            }),
            page: 0,
            origin: (x, y + 8.0),
            rotation_deg: 0.0,
            font_size: 8.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        let prose = [text(60.0, 200.0), text(320.0, 200.0)];
        // A full-width table's rule under its last row.
        assert!(ruled_across_gutter(
            &g,
            &prose,
            &[h(40.0, 560.0, 183.0)],
            300.0
        ));
        // A group of a table's rows read on its own, ruled over its first
        // line and, past a taller cell's last lines, under its last.
        let round = [h(40.0, 560.0, 96.0), h(40.0, 560.0, 205.0)];
        assert!(ruled_across_gutter(&g, &[], &round, 300.0));
        // The same rule drawn a cell at a time, its pieces meeting.
        let pieces = [h(40.0, 300.0, 183.0), h(300.5, 560.0, 183.0)];
        assert!(ruled_across_gutter(&g, &prose, &pieces, 300.0));
        // Two tables side by side, each ruled to its own edge: the gutter's
        // white parts their rules, however wide each is.
        let apart = [h(40.0, 290.0, 183.0), h(310.0, 560.0, 183.0)];
        assert!(!ruled_across_gutter(&g, &prose, &apart, 300.0));
        // A rule under one column's heading, run a little past the gutter.
        assert!(!ruled_across_gutter(
            &g,
            &prose,
            &[h(60.0, 320.0, 183.0)],
            300.0
        ));
        // A ruled table over the reading's first rows only, the reading run
        // on into prose: no rule at its foot.
        assert!(!ruled_across_gutter(
            &g,
            &prose,
            &[h(40.0, 560.0, 120.0)],
            300.0
        ));
        // The rule over a page's foot, set against the foot's line.
        let foot = [text(60.0, 192.0)];
        assert!(!ruled_across_gutter(
            &g,
            &foot,
            &[h(40.0, 560.0, 190.0)],
            300.0
        ));
    }

    #[test]
    fn a_wide_value_standing_in_alike_from_both_edges_is_spread() {
        // A label column and two columns a value is set across (x 60–260),
        // its text at 10 pt.
        let g = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 60.0, 160.0, 260.0],
            ys: vec![0.0, 20.0, 40.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 260.0,
                y1: 40.0,
            },
        };
        let rows: Vec<Vec<String>> = vec![
            vec!["Spec".into(), "A".into(), "B".into()],
            vec![
                "Warranty".into(),
                "two years on parts".into(),
                String::new(),
            ],
        ];
        let left = vec![false, false, false, false, false, true];
        let ink = |x0: f64, x1: f64| Glyph {
            text: "two years on parts".into(),
            bbox: Some(BBox {
                x0,
                y0: 25.0,
                x1,
                y1: 35.0,
            }),
            page: 0,
            origin: (x0, 35.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        let spread = |x0: f64, x1: f64| spread_runs(&g, &[ink(x0, x1)], &rows, &left)[4];
        // Narrow and centred, as before.
        assert!(spread(110.0, 210.0));
        // 85% of the run, 15 pt in from each edge: centred as surely.
        assert!(spread(75.0, 245.0));
        // As wide, set from the left: in by the padding at its start.
        assert!(!spread(63.0, 233.0));
        // Filling the run: in alike, but by the padding alone, under an em.
        assert!(!spread(63.0, 257.0));
        // As wide, an em or more in from each edge, but not alike.
        assert!(!spread(71.0, 241.0));
    }

    #[test]
    fn a_band_over_all_but_a_margin_column_is_the_tables_title() {
        use crate::rule::{Orientation, Rule};
        let word = |x0: f64, x1: f64, y: f64, t: &str, i: usize| Glyph {
            text: t.into(),
            bbox: Some(BBox {
                x0,
                y0: y - 7.0,
                x1,
                y1: y,
            }),
            page: 0,
            origin: (x0, y),
            rotation_deg: 0.0,
            font_size: 8.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: i,
            ocr: None,
        };
        // A grid whose first column is `first` wide; the band over the rest
        // of it, the rule at the first column's edge running up through it.
        let table = |first: f64| {
            let xs = vec![0.0, first, first + 50.0, first + 100.0];
            let ys = vec![0.0, 15.0, 30.0, 45.0, 60.0];
            let right = xs[3];
            let mut rules: Vec<Rule> = ys
                .iter()
                .map(|&y| Rule {
                    bbox: BBox {
                        x0: 0.0,
                        y0: y,
                        x1: right,
                        y1: y + 0.5,
                    },
                    orientation: Orientation::Horizontal,
                    page: 0,
                })
                .collect();
            let v = |x: f64, y0: f64| Rule {
                bbox: BBox {
                    x0: x,
                    y0,
                    x1: x + 0.5,
                    y1: 60.0,
                },
                orientation: Orientation::Vertical,
                page: 0,
            };
            rules.extend([v(0.0, 0.0), v(first, 0.0), v(right, 0.0)]);
            rules.extend([v(first + 50.0, 15.0)]);
            let mut glyphs = vec![
                word(first + 4.0, first + 40.0, 11.0, "Hourly", 0),
                word(first + 43.0, first + 80.0, 11.0, "readings", 1),
            ];
            for (r, row) in [
                ["Site", "North", "South"],
                ["1", "3.2", "4.1"],
                ["2", "3.5", "4.4"],
            ]
            .iter()
            .enumerate()
            {
                let y = 26.0 + r as f64 * 15.0;
                glyphs.push(word(2.0, 10.0, y, row[0], 2 + r * 3));
                glyphs.push(word(first + 4.0, first + 30.0, y, row[1], 3 + r * 3));
                glyphs.push(word(first + 54.0, first + 80.0, y, row[2], 4 + r * 3));
            }
            let grid = Grid {
                layout: None,
                page: 0,
                bbox: BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: right,
                    y1: 60.0,
                },
                xs,
                ys,
            };
            let page = crate::extract::Page {
                index: 0,
                glyphs,
                images: Vec::new(),
                rules,
                fills: Vec::new(),
                dots: Vec::new(),
                traces: Vec::new(),
                ticks: Vec::new(),
                width: 600.0,
                height: 800.0,
                rotation: 0,
                ocr: None,
            };
            (grid, page)
        };
        // A first column 12 pt wide at 8 pt text: room for a turned line,
        // not a label set across it. The band is the table's.
        let (g, page) = table(12.0);
        let titled = table_title(&g, &page, &[], false);
        assert_eq!(titled.map(|(t, _)| t).as_deref(), Some("Hourly readings"));
        // A first column a label fits beside it: a group's label over part
        // of the table.
        let (g, page) = table(60.0);
        assert!(table_title(&g, &page, &[], false).is_none());
    }

    #[test]
    fn rows_of_parameters_head_a_grid_of_figures() {
        let rows = |rs: &[&[&str]]| -> Vec<Vec<String>> {
            rs.iter()
                .map(|r| r.iter().map(|t| t.to_string()).collect())
                .collect()
        };
        let cu = rows(&[
            &["RC %:", "80", "", ""],
            &["RW %:", "70", "50", "30"],
            &["RCR: 0", "119", "119", "119"],
            &["1", "106", "102", "98"],
            &["2", "95", "88", "81"],
        ]);
        assert_eq!(parameter_rows(&cu), 2);
        // A sweep: the speed each column of readings is taken at.
        let sweep = rows(&[
            &["Speed (rpm):", "1500", "2500", "3500"],
            &["Torque (N·m)", "5.1", "4.8", "4.2"],
            &["Power (kW)", "0.5", "1.0", "1.3"],
            &["Current (A)", "1.1", "2.0", "2.9"],
        ]);
        assert_eq!(parameter_rows(&sweep), 1);
        // Fields over a grid of figures, a value each: they head nothing.
        let fields_over = rows(&[
            &["Voltage:", "120", "", ""],
            &["Watts:", "16", "", ""],
            &["Size", "1", "2", "3"],
            &["Width", "10", "20", "30"],
            &["Depth", "12", "24", "36"],
        ]);
        assert_eq!(parameter_rows(&fields_over), 0);
        // Values beside the columns the body fills name none of them.
        let beside = rows(&[
            &["Batch:", "", "", "", "7"],
            &["Lot:", "", "", "4", "9"],
            &["A", "1", "2", "", ""],
            &["B", "3", "4", "", ""],
            &["C", "5", "6", "", ""],
        ]);
        assert_eq!(parameter_rows(&beside), 0);
        // Fields all the way down: none heads the rest.
        let fields = rows(&[
            &["Voltage:", "120", "", ""],
            &["Watts:", "16", "", ""],
            &["Lumens:", "1200", "", ""],
        ]);
        assert_eq!(parameter_rows(&fields), 0);
        // Too few rows of figures under them.
        assert_eq!(parameter_rows(&cu[..4]), 0);
        // A label without its colon names a row, not a parameter.
        let mut plain = cu.clone();
        plain[0][0] = "RC %".into();
        assert_eq!(parameter_rows(&plain), 0);
    }

    #[test]
    fn a_forms_text_stays_in_its_run_of_bold_heads_until_judged() {
        let head = |t: &str, ev: &'static str, y: f64| {
            line_at("doco:SectionTitle", t, ev, Some(2), y, 50.0)
        };
        let text = |t: &str, y: f64| line_at("doco:Paragraph", t, "layout", None, y, 50.0);
        // A grid's caption, its keys, and another caption, stacked: a run
        // of three, all but the last text. Dropped first, the keys would
        // leave a run of two, both kept.
        let mut grid = vec![
            head("Description of property", "bold", 100.0),
            head("(a) (b)", "form-bold", 112.0),
            head("Gain or (loss)", "bold", 124.0),
            text("Example 100 sh. XYZ Co.", 140.0),
        ];
        demote_empty_bold_heads(&mut grid);
        read_form_text(&mut grid);
        assert_eq!(heads(&grid), ["Gain or (loss)"]);
        assert!(grid.iter().all(|e| !e.evidence.starts_with(FORM_TEXT)));
    }

    #[test]
    fn a_bold_line_over_a_larger_head_or_closing_its_paragraph_is_text() {
        let head = |t: &str, ev: &'static str, lvl: usize, y: f64| {
            line_at("doco:SectionTitle", t, ev, Some(lvl), y, 50.0)
        };
        let text = |t: &str, y: f64| line_at("doco:Paragraph", t, "layout", None, y, 50.0);
        // A slide's kicker over its title, set larger.
        let mut deck = vec![
            head("Quarterly review", "title", 1, 30.0),
            text("A short note under the title.", 52.0),
            head("PART 2 · OUTLOOK", "bold", 4, 74.0),
            head("What the next quarter asks of us", "font-size", 2, 92.0),
            text("Text the slide sets under its title…", 110.0),
        ];
        demote_empty_bold_heads(&mut deck);
        assert_eq!(
            heads(&deck),
            ["Quarterly review", "What the next quarter asks of us"]
        );
        // The document's first heading may be its title over a larger date.
        let mut transcript = vec![
            head("Record of the Annual Meeting", "bold", 4, 70.0),
            head("March 3, 2031", "font-size", 2, 88.0),
            text("THE CHAIR. The meeting will come to order.", 106.0),
        ];
        demote_empty_bold_heads(&mut transcript);
        assert_eq!(heads(&transcript).len(), 2);
        // A warning's bold last line, at its leading, over the next head.
        let mut warning = vec![
            head("WARNING", "bold", 5, 200.0),
            text("Handle with gloves and wash hands after use…", 212.0),
            head("STORE IN A DRY PLACE", "bold", 5, 224.0),
            head("DISPOSAL", "bold", 5, 241.0),
            text("Dispose of in line with local rules.", 253.0),
        ];
        demote_empty_bold_heads(&mut warning);
        assert_eq!(heads(&warning), ["WARNING", "DISPOSAL"]);
        // Set apart from the paragraph above, a section opening on its
        // first subsection keeps both heads.
        let mut paper = vec![
            text("…ter analyses was obtained.", 60.0),
            head("Materials and methods", "bold", 3, 90.0),
            head("Animals", "bold", 3, 105.0),
            text("Mice were obtained…", 120.0),
        ];
        demote_empty_bold_heads(&mut paper);
        assert_eq!(heads(&paper), ["Materials and methods", "Animals"]);
    }

    #[test]
    fn a_heading_centred_over_two_lines_is_one() {
        let centred = |t: &str, x0: f64, x1: f64, y: f64| {
            let mut e = line_at("doco:SectionTitle", t, "font-size", Some(3), y, x0);
            e.bbox = Some(BBox {
                x0,
                y0: y,
                x1,
                y1: y + 12.0,
            });
            e
        };
        // A subtitle over two centred lines, about a page centre of 300.
        let mut subtitle = vec![
            centred("Recycled Fibre Board", 250.0, 350.0, 140.0),
            centred("Rated for use in damp interiors", 205.0, 395.0, 155.0),
        ];
        coalesce_heading_fragments(&mut subtitle);
        assert_eq!(
            heads(&subtitle),
            ["Recycled Fibre Board Rated for use in damp interiors"]
        );
        // Cells of a table no grid took, centred in their column, with
        // their row's other cells beside them.
        let mut cells = vec![
            line_at("doco:Paragraph", "Capacity", "layout", None, 420.0, 60.0),
            centred("12 litres", 330.0, 370.0, 420.0),
            line_at("doco:Paragraph", "Weight", "layout", None, 434.0, 60.0),
            centred("2.5 kg (empty)", 320.0, 380.0, 434.0),
        ];
        coalesce_heading_fragments(&mut cells);
        assert_eq!(heads(&cells).len(), 2);
        // Set from one left edge, a section and its first subsection.
        let mut pair = vec![
            centred("Materials and methods", 200.0, 330.0, 100.0),
            centred("Animals", 200.0, 250.0, 114.0),
        ];
        coalesce_heading_fragments(&mut pair);
        assert_eq!(heads(&pair).len(), 2);
    }

    #[test]
    fn bold_lines_that_head_nothing_are_text() {
        let head = |t: &str, y: f64, x: f64| line_at("doco:SectionTitle", t, "bold", Some(2), y, x);
        let text = |t: &str, y: f64| line_at("doco:Paragraph", t, "layout", None, y, 50.0);
        // A section, then the name and role closing the minutes.
        let mut minutes = vec![
            head("Notation Vote", 100.0, 50.0),
            text("By notation vote completed on…", 115.0),
            head("Joshua Gallin", 160.0, 50.0),
            head("Secretary", 172.0, 50.0),
        ];
        demote_empty_bold_heads(&mut minutes);
        assert_eq!(heads(&minutes), ["Notation Vote"]);
        // A table's header cells read across one row, then its rows.
        let mut cells = vec![
            head("TYPE", 100.0, 50.0),
            head("DESCRIPTION", 100.0, 150.0),
            text("I Ground reference voltage", 115.0),
        ];
        demote_empty_bold_heads(&mut cells);
        assert!(heads(&cells).is_empty());
        // A list of names, one under another: the last is followed by text.
        let mut names = vec![
            head("Ann Archer", 100.0, 50.0),
            head("Ben Baker", 112.0, 50.0),
            head("Cara Cole", 124.0, 50.0),
            text("Address for notices", 140.0),
        ];
        demote_empty_bold_heads(&mut names);
        assert_eq!(heads(&names), ["Cara Cole"]);
        // A section opening on its first subsection, set the same way.
        let mut paper = vec![
            head("Materials and Methods", 100.0, 50.0),
            head("Apiary selection", 115.0, 50.0),
            text("Colonies were drawn from…", 130.0),
        ];
        demote_empty_bold_heads(&mut paper);
        assert_eq!(heads(&paper), ["Materials and Methods", "Apiary selection"]);
        // A division's label beside its title, over its table: one heading.
        let mut form = vec![
            head("Part I", 100.0, 50.0),
            head("Annual and Monthly Contribution Amount", 100.0, 140.0),
            text("1 Tax family size", 115.0),
        ];
        demote_empty_bold_heads(&mut form);
        assert_eq!(
            heads(&form),
            ["Part I", "Annual and Monthly Contribution Amount"]
        );
        // A heading of another kind after a bold line says nothing of it.
        let mut outlined = vec![
            head("Sectors of primary focus", 100.0, 50.0),
            line_at(
                "doco:SectionTitle",
                "Energy",
                "outline",
                Some(1),
                115.0,
                50.0,
            ),
            text("Electricity generation…", 130.0),
        ];
        demote_empty_bold_heads(&mut outlined);
        assert_eq!(heads(&outlined), ["Sectors of primary focus", "Energy"]);
    }

    #[test]
    fn a_word_a_cell_breaks_is_read_whole_where_the_document_prints_it_whole() {
        let brk = table::CELL_BREAK;
        let cells = vec![vec![
            format!("3-wire Fluo{brk}rescent Bal{brk}lasts"),
            "20 Ballasts".to_string(),
            format!("long{brk}term"),
            format!("Un{brk}employ{brk}ment"),
            format!("Wid{brk}gets"),
            // A note's number raised after the word is no part of it.
            format!("consoli{brk}dated²"),
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
            "consolidated figures",
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
                "consolidated²",
            ]
        );
        assert_eq!(
            elements[1].text,
            "3-wire Fluorescent Ballasts | 20 Ballasts | long-term | Unemployment | Wid-gets \
             | consolidated²"
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
    fn a_table_nested_in_a_label_s_rows_opens_on_its_header() {
        let rows = grid(&[
            &["Flow Rate", "0.5 mL/min", "", ""],
            &["Gradient", "Time (min)", "A%", "B%"],
            &["", "0", "95", "5"],
            &["", "0.7", "50", "50"],
            &["", "8.0", "10", "90"],
            &["Injection Volume", "5 µL", "", ""],
        ]);
        let mut down = vec![false; 24];
        for r in 2..5 {
            down[r * 4] = true;
        }
        assert_eq!(nested_header_rows(&rows, &down, 0), vec![1]);
        // A subgroup's counts are values, not names.
        let mut counts = rows.clone();
        counts[1] = ["Responders", "N=175", "N=92", "N=40"]
            .map(String::from)
            .to_vec();
        assert!(nested_header_rows(&counts, &down, 0).is_empty());
        // A group's first row of data holds figures among its values.
        let mut group = rows.clone();
        group[1] = ["North", "Sales", "95", "5"].map(String::from).to_vec();
        assert!(nested_header_rows(&group, &down, 0).is_empty());
        // A label over one row is no span of rows to head.
        let mut short = vec![false; 24];
        short[2 * 4] = true;
        assert!(nested_header_rows(&rows, &short, 0).is_empty());
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
        assert!(names_spanned_columns(&methods, &left, &[]));
        // Data under a banner repeats itself; it names nothing.
        let data = grid(&[
            &["", "Primary", "", "Secondary", ""],
            &["Channel", "Short", "Short", "Short", "Short"],
            &["Flow", "0.5", "0.5", "1.5", "1.5"],
        ]);
        assert!(!names_spanned_columns(&data, &left, &[]));
        // Nor is a row of numbers, nor a header with no span.
        let numbers = grid(&[
            &["", "Primary", "", "Secondary", ""],
            &["Flow", "0.5", "0.7", "1.5", "1.7"],
            &["Time", "3", "4", "5", "6"],
        ]);
        assert!(!names_spanned_columns(&numbers, &left, &[]));
        assert!(!names_spanned_columns(&methods, &[false; 15], &[]));
        // Keys in figures under a span name its columns where their own
        // stub is empty and the rows under them have one: sizes over a
        // model's readings.
        let mut size_left = vec![false; 16];
        size_left[2] = true;
        size_left[3] = true;
        let sizes = grid(&[
            &["", "Width (mm)", "", ""],
            &["", "100", "150", "200"],
            &["Model A", "2.1", "2.9", "3.8"],
            &["Model B", "2.4", "3.3", "4.2"],
        ]);
        assert!(names_spanned_columns(&sizes, &size_left, &[]));
        // With no stub under them, they are a row of the data like the rest.
        let unstubbed = grid(&[
            &["", "Width (mm)", "", ""],
            &["", "100", "150", "200"],
            &["", "2.1", "2.9", "3.8"],
            &["", "2.4", "3.3", "4.2"],
        ]);
        assert!(!names_spanned_columns(&unstubbed, &size_left, &[]));
        // Nor do keys that repeat.
        let repeated = grid(&[
            &["", "Width (mm)", "", ""],
            &["", "100", "100", "200"],
            &["Model A", "2.1", "2.9", "3.8"],
            &["Model B", "2.4", "3.3", "4.2"],
        ]);
        assert!(!names_spanned_columns(&repeated, &size_left, &[]));
        // A span the ruling leaves open over the stub's column too names
        // the columns from where its label stands.
        let mut wide = vec![false; 20];
        wide[2] = true;
        wide[3] = true;
        wide[4] = true;
        let angles = grid(&[
            &["", "", "Horizontal angle", "", ""],
            &["", "", "0°", "45°", "90°"],
            &["Vertical angle", "45°", "23,252", "23,208", "22,970"],
            &["", "55°", "17,172", "17,146", "16,972"],
        ]);
        assert!(names_spanned_columns(&angles, &wide, &[]));
        // A value run across the banner's columns names nothing.
        let mut run = left.clone();
        run[5 + 2] = true;
        assert!(!names_spanned_columns(&methods, &run, &[]));
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
        assert!(!names_spanned_columns(&titled, &band, &[]));
        let named = grid(&[
            &["Establishment", "", ""],
            &["Name", "Address", "ID/FEI"],
            &["Plant A", "1 Main St", "123"],
        ]);
        assert!(names_spanned_columns(&named, &band, &[]));
        // A label across two columns over a product's row: the columns it
        // does not span hold the product's own codes, so the row is data.
        let mut typed = vec![false; 12];
        typed[1] = true;
        let products = grid(&[
            &["Type", "", "Product No.", "Part No."],
            &[
                "Japanese version kit",
                "CD-ROM, with cable",
                "FPWINGRF-JP2",
                "AFPS10122",
            ],
            &["English version", "CD-ROM", "FPWINGRF-EN2", "AFPS10520"],
        ]);
        assert!(!names_spanned_columns(&products, &typed, &[]));
        // Unless the ruling runs those columns' labels down into the row.
        let mut down = vec![false; 12];
        down[4 + 2] = true;
        down[4 + 3] = true;
        let stacked = grid(&[
            &["Type", "", "Product No.", "Part No."],
            &["Medium", "Cable", "", "No."],
            &["English version", "CD-ROM", "FPWINGRF-EN2", "AFPS10520"],
        ]);
        assert!(names_spanned_columns(&stacked, &typed, &down));
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
        // A group over its sub-group, both over the rows they label.
        let stacked = grid(&[
            &["Model", "A", "B"],
            &["Power Management", "", ""],
            &["Average power requirements (W)", "", ""],
            &["Operational", "6.7W", "6.7W"],
        ]);
        assert!(group_label(&stacked, 1));
        assert!(group_label(&stacked, 2));
        // Not a label over a row left empty at the table's end.
        let dangling = grid(&[
            &["Model", "A", "B"],
            &["Notes", "", ""],
            &["See below", "", ""],
        ]);
        assert!(!group_label(&dangling, 1));
    }

    #[test]
    fn a_blank_row_inside_a_table_is_not_emitted() {
        let mut t = Element {
            cells: Some(grid(&[
                &["Mode", "Constant flow"],
                &["", ""],
                &["Headspace", ""],
                &["Volume", "1000 µL"],
                &["Spanned", "x"],
                &["", ""],
                &["Last", "y"],
            ])),
            header_rows: Some(0),
            sub_headers: Some(vec![2]),
            // The second blank row is the merged cell's, set over two rows.
            merged_down: Some(vec![
                false, false, false, false, false, false, false, false, false, false, true, false,
                false, false,
            ]),
            ..Element::new("doco:Table", "", "rust", "rules")
        };
        drop_blank_rows(&mut t);
        assert_eq!(
            t.cells.as_deref(),
            Some(
                &grid(&[
                    &["Mode", "Constant flow"],
                    &["Headspace", ""],
                    &["Volume", "1000 µL"],
                    &["Spanned", "x"],
                    &["", ""],
                    &["Last", "y"],
                ])[..]
            )
        );
        assert_eq!(t.sub_headers, Some(vec![1]));
        assert_eq!(t.merged_down.as_ref().map(Vec::len), Some(12));
        assert_eq!(t.merged_down.as_ref().map(|m| m[8]), Some(true));
        assert!(t.text.starts_with("Mode | Constant flow\nHeadspace | "));
    }

    #[test]
    fn a_tables_reading_is_in_doubt_unless_one_plain_row_heads_it() {
        let plain = Element {
            cells: Some(grid(&[&["Name", "Count"], &["alpha", "12"]])),
            header_rows: Some(1),
            ..Element::new("doco:Table", "Name | Count\nalpha | 12", "rust", "rules")
        };
        assert!(!table_in_doubt(&plain, false));
        // Its structure disagreeing with itself puts it in doubt whatever
        // heads it.
        assert!(table_in_doubt(&plain, true));
        let flags = Some(vec![false, true, false, false]);
        for doubted in [
            Element {
                header_rows: Some(0),
                ..plain.clone()
            },
            Element {
                header_rows: Some(2),
                ..plain.clone()
            },
            Element {
                merged_down: flags.clone(),
                ..plain.clone()
            },
            Element {
                merged_left: flags,
                ..plain.clone()
            },
            Element {
                sub_headers: Some(vec![1]),
                ..plain.clone()
            },
            Element {
                restated_headers: Some(vec![1]),
                ..plain.clone()
            },
        ] {
            assert!(table_in_doubt(&doubted, false), "{doubted:?}");
        }
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
        // Every bullet a list item opens with, not only the commonest.
        for bullet in ["●", "■", "○", "➢", "\u{F0B7}"] {
            assert!(
                is_marker_list(&cells(&[[bullet, "One."], [bullet, "Two."]]), 2),
                "{bullet}"
            );
        }
        // A column of one letter or sign is no column of bullets.
        assert!(!is_marker_list(&cells(&[["x", "One."], ["x", "Two."]]), 2));
        assert!(!is_marker_list(
            &cells(&[["●●", "One."], ["●●", "Two."]]),
            2
        ));
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

    /// A grid read from alignment, `rows` of cells in columns 100 pt wide
    /// and rows 20 pt tall, and the glyphs in it, a word each.
    fn aligned_grid(rows: &[&[&str]]) -> (Grid, Vec<Glyph>) {
        let cols = rows[0].len();
        let xs: Vec<f64> = (0..=cols).map(|c| c as f64 * 100.0).collect();
        let ys: Vec<f64> = (0..=rows.len()).map(|r| r as f64 * 20.0).collect();
        let mut glyphs = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, word) in row.iter().enumerate() {
                let (x, y) = (c as f64 * 100.0 + 5.0, r as f64 * 20.0 + 14.0);
                glyphs.push(Glyph {
                    text: (*word).into(),
                    bbox: Some(BBox {
                        x0: x,
                        y0: y - 8.0,
                        x1: x + 6.0 * word.chars().count() as f64,
                        y1: y,
                    }),
                    page: 0,
                    origin: (x, y),
                    rotation_deg: 0.0,
                    font_size: 8.0,
                    weight: None,
                    italic: false,
                    advance: None,
                    draw_index: glyphs.len(),
                    ocr: None,
                });
            }
        }
        let grid = Grid {
            layout: None,
            page: 0,
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: xs[cols],
                y1: ys[rows.len()],
            },
            xs,
            ys,
        };
        (grid, glyphs)
    }

    #[test]
    fn a_bulleted_list_lined_up_as_a_table_is_a_list() {
        // Its bullets, and its items beside them.
        let (g, glyphs) = aligned_grid(&[&["•", "Pipettes"], &["•", "Rack"], &["•", "Tray"]]);
        assert!(lists_its_rows(&g, &glyphs));
        // Under a line set level with it: a running head, a heading.
        let (g, glyphs) = aligned_grid(&[
            &["COLLEGE", "BIO181"],
            &["●", "Pipettes"],
            &["●", "Rack"],
            &["●", "Tray"],
        ]);
        assert!(lists_its_rows(&g, &glyphs));
        // A numbered or lettered list stays as it was read: its items, read
        // back as prose, would be numbered lines.
        let (g, glyphs) = aligned_grid(&[&["1.", "Thaw."], &["2.", "Mix."], &["3.", "Spin."]]);
        assert!(!lists_its_rows(&g, &glyphs));
        let (g, glyphs) = aligned_grid(&[&["(a)", "Board"], &["(b)", "Code"], &["(c)", "Holder"]]);
        assert!(!lists_its_rows(&g, &glyphs));
        // Two rows of other text over the markers head a table.
        let (g, glyphs) = aligned_grid(&[
            &["Step", "Action"],
            &["When", "Done"],
            &["•", "Rack"],
            &["•", "Tray"],
        ]);
        assert!(!lists_its_rows(&g, &glyphs));
        // A third column is more than a list's items: numbered rows of a
        // table.
        let (g, glyphs) = aligned_grid(&[
            &["1.", "Widget", "4.50"],
            &["2.", "Gadget", "3.10"],
            &["3.", "Sprocket", "1.25"],
        ]);
        assert!(!lists_its_rows(&g, &glyphs));
        // Labels down the first column are a table's.
        let (g, glyphs) = aligned_grid(&[&["Width", "40"], &["Depth", "25"], &["Height", "90"]]);
        assert!(!lists_its_rows(&g, &glyphs));
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
    fn a_header_boxed_over_its_rows_is_folded_into_them() {
        // A header filled in a box of its own, its rows ruled in another a
        // few points below, on the same columns.
        let (mut a, mut fa) = part(
            "elem-00001",
            0,
            &[&["Size", "Diameter", "Length"]],
            1,
            &XS,
            false,
            false,
        );
        let (mut b, mut fb) = part(
            "elem-00002",
            0,
            &[&["5", "0.125", "0.50"], &["6", "0.138", "0.75"]],
            0,
            &XS,
            false,
            false,
        );
        a.bbox = Some(BBox {
            x0: 72.0,
            y0: 600.0,
            x1: 595.0,
            y1: 620.0,
        });
        b.bbox = Some(BBox {
            x0: 72.0,
            y0: 626.0,
            x1: 595.0,
            y1: 720.0,
        });
        fa.bbox = a.bbox.unwrap();
        fb.bbox = b.bbox.unwrap();
        let mut els = vec![a, b];
        link_continued_tables(&mut els, &[fa, fb]);
        assert_eq!(els[1].continues.as_deref(), Some("elem-00001"));
        fold_header_boxes(&mut els);
        assert_eq!(els.len(), 1, "{els:?}");
        let t = &els[0];
        assert_eq!(t.id, "elem-00002");
        assert_eq!(t.header_rows, Some(1));
        assert_eq!(t.continues, None);
        assert_eq!(t.cells.as_ref().unwrap()[0], ["Size", "Diameter", "Length"]);
        assert_eq!(t.cells.as_ref().unwrap().len(), 3);
        assert_eq!(t.bbox.unwrap().y0, 600.0);
        // A part on the page before is left linked: one table across a page
        // break is two elements, one per page.
        let (c, _) = part(
            "elem-00003",
            0,
            &[&["Size", "Diameter", "Length"]],
            1,
            &XS,
            false,
            false,
        );
        let (mut d, _) = part(
            "elem-00004",
            1,
            &[&["7", "0.164", "1.00"]],
            0,
            &XS,
            false,
            false,
        );
        d.continues = Some("elem-00003".into());
        let mut els = vec![c, d];
        fold_header_boxes(&mut els);
        assert_eq!(els.len(), 2);
    }

    #[test]
    fn a_splice_anchor_between_the_parts_keeps_them_one_table() {
        let (a, fa) = part(
            "elem-00001",
            0,
            &[&["Name", "Value"], &["alpha", "1"]],
            1,
            &XS,
            false,
            false,
        );
        let (b, fb) = part(
            "elem-00003",
            1,
            &[&["beta", "2"], &["gamma", "3"]],
            1,
            &XS,
            false,
            false,
        );
        // The second part's own anchor, ordered just before it.
        let anchor = Element {
            id: "elem-00002".into(),
            page: 1,
            bbox: b.bbox,
            ..Element::new(
                "doco:Figure",
                "[[VLMTAB:p1:t0]]",
                "rust",
                "table-confidence",
            )
        };
        let mut els = vec![a, anchor, b];
        link_continued_tables(&mut els, &[fa, fb]);
        assert_eq!(els[2].continues.as_deref(), Some("elem-00001"));
        assert_eq!(els[2].header_rows, Some(0));
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
            italic: false,
            ocr: None,
            x_height: None,
            compounds: Vec::new(),
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
    fn a_sure_titles_subtitle_follows_it_and_its_kickers_stand_over_it() {
        // A block of `lines` lines at `fs` points, its top at `y`.
        let block = |fs: f32, y: f64, lines: usize, bold: bool| -> Block {
            let bbox = BBox {
                x0: 50.0,
                y0: y,
                x1: 400.0,
                y1: y + fs as f64 * 1.2 * lines as f64,
            };
            let line = Line {
                text: String::new(),
                bbox,
                page: 0,
                rotation_bucket: 0,
                glyphs: vec![],
                font_size: fs,
                bold,
                opens_with_script: false,
                italic: false,
                ocr: None,
                x_height: None,
                compounds: Vec::new(),
                opens_bold: false,
            };
            Block {
                lines: vec![line; lines],
                bbox,
                page: 0,
                font_size: fs,
                bold,
                gap_above: None,
                marker: None,
                marker_x: None,
                stacked: false,
                field: false,
            }
        };
        let el = |kind: &str, text: &str, evidence: &'static str| Element {
            page: 0,
            level: (kind == "doco:SectionTitle").then_some(1),
            ..Element::new(kind, text, "rust", evidence)
        };
        // Kicker, title, the line under it, a section's heading and its text,
        // each line's block by its text.
        let settle = |under: (&str, f32, f64, usize, bool), title: &str, meta: Option<&str>| {
            let blocks: std::collections::HashMap<String, Block> = [
                (title.to_string(), block(24.0, 100.0, 1, true)),
                (
                    under.0.to_string(),
                    block(under.1, under.2, under.3, under.4),
                ),
                ("Overview".to_string(), block(14.0, 300.0, 1, true)),
                ("Outlook".to_string(), block(14.0, 500.0, 1, true)),
            ]
            .into_iter()
            .collect();
            let mut els = vec![
                el("doco:Paragraph", "Policy series", "kicker"),
                el("doco:SectionTitle", title, "title"),
                el("doco:SectionTitle", under.0, "font-size"),
                el("doco:SectionTitle", "Overview", "font-size"),
                el("doco:Paragraph", "The report's text.", "layout"),
                el("doco:SectionTitle", "Outlook", "font-size"),
            ];
            settle_title(&mut els, meta, &[], |e| blocks.get(&e.text), 10.0);
            els.into_iter()
                .map(|e| (e.kind, e.evidence))
                .collect::<Vec<_>>()
        };
        let report = "Monetary Policy Report";
        let kinds = settle(("July 2026", 16.0, 130.0, 1, false), report, Some(report));
        assert_eq!(
            kinds[..3],
            [
                ("doco:Paragraph".to_string(), "kicker"),
                ("doco:Title".to_string(), "metadata"),
                ("doco:Subtitle".to_string(), "subtitle"),
            ]
        );
        // The sections stay headings.
        assert_eq!(kinds[3], ("doco:SectionTitle".to_string(), "font-size"));
        // Not a subtitle: far under the title, as large as it, over three
        // lines, numbered, or the authors' names.
        for under in [
            ("Grace E. Rice", 14.0, 130.0, 1, false),
            ("July 2026", 16.0, 200.0, 1, false),
            ("July 2026", 24.0, 130.0, 1, false),
            ("A line run on over three lines", 16.0, 130.0, 3, false),
            ("1. Purpose", 16.0, 130.0, 1, false),
        ] {
            let kinds = settle(under, report, Some(report));
            assert_eq!(kinds[2].0, "doco:SectionTitle", "{under:?}");
        }
        // Under a line in doubt there is no title: no subtitle, and the
        // kicker is text.
        let kinds = settle(
            ("Opening remarks", 16.0, 130.0, 1, false),
            "September 20, 2023",
            None,
        );
        assert_eq!(kinds[0], ("doco:Paragraph".to_string(), "layout"));
        assert_eq!(kinds[1].0, "doco:SectionTitle");
        assert_eq!(kinds[2].0, "doco:SectionTitle");
    }

    #[test]
    fn the_title_is_the_documents_only_where_it_is_sure() {
        let at = |kind: &str, text: &str, evidence: &'static str| Element {
            page: 0,
            level: (kind == "doco:SectionTitle").then_some(1),
            ..Element::new(kind, text, "rust", evidence)
        };
        let settle = |mut els: Vec<Element>, meta: Option<&str>| {
            settle_title(&mut els, meta, &[], |_| None, 10.0);
            els.into_iter()
                .map(|e| (e.kind, e.text, e.evidence))
                .collect::<Vec<_>>()
        };
        // The file's own title names it.
        let els = settle(
            vec![at("doco:SectionTitle", "Monetary Policy Report", "title")],
            Some("Monetary Policy Report, July 2026"),
        );
        assert_eq!(
            els[0],
            (
                "doco:Title".into(),
                "Monetary Policy Report".into(),
                "metadata"
            )
        );
        // Set apart by its type, nothing doubting it.
        let els = settle(
            vec![at(
                "doco:SectionTitle",
                "Pipette Performance Data Sheet",
                "title",
            )],
            None,
        );
        assert_eq!(els[0].2, "type");
        // A date is no title: a heading the file's title names is.
        let els = settle(
            vec![
                at(
                    "doco:SectionTitle",
                    "Transcript of Chair Powell's Press Conference",
                    "bold",
                ),
                at("doco:SectionTitle", "September 20, 2023", "title"),
            ],
            Some("Transcript of Chair Powell's Press Conference -- September 20, 2023"),
        );
        assert_eq!(els[0].0, "doco:Title");
        assert_eq!(
            (els[1].0.as_str(), els[1].2),
            ("doco:SectionTitle", "font-size")
        );
        // A field's line is no title, though the file's title names its value.
        let els = settle(
            vec![
                at(
                    "doco:SectionTitle",
                    "Final Details for Order #114-0619372-9653019",
                    "title",
                ),
                at(
                    "doco:Paragraph",
                    "Amazon.com order number: 114-0619372-9653019",
                    "layout",
                ),
            ],
            Some("Amazon.com - Order 114-0619372-9653019"),
        );
        assert_eq!(els[0].0, "doco:Title");
        assert_eq!(els[1].0, "doco:Paragraph");
        // A company's name in doubt stays a heading, and no title is read.
        let els = settle(
            vec![at(
                "doco:SectionTitle",
                "Hanvaro Semiconductor Co., Ltd.",
                "title",
            )],
            None,
        );
        assert_eq!(
            (els[0].0.as_str(), els[0].2),
            ("doco:SectionTitle", "font-size")
        );
    }

    #[test]
    fn a_heading_continuing_an_earlier_one_is_no_new_section() {
        let at = |kind: &str, text: &str, page: usize, level: Option<usize>| {
            let mut e = Element::new(kind, text, "rust", "layout");
            e.page = page;
            e.level = level;
            e
        };
        let head = |text: &str, page: usize, level: usize| {
            at("doco:SectionTitle", text, page, Some(level))
        };
        let text = |page: usize| at("doco:Paragraph", "Its text runs on.", page, None);
        let mut els = vec![
            head("Box 1. Employment across Groups", 0, 2),
            text(0),
            head("Box 1—continued", 1, 4),
            text(1),
            head("Wiring Diagrams", 1, 1),
            head("Wiring Diagrams (continued)", 2, 1),
            head("PACKAGE OPTION ADDENDUM", 3, 1),
            head("Orderable parts", 3, 2),
            head("PACKAGE OPTION ADDENDUM", 4, 1),
            // A district's own `Labor Markets`, under another district.
            head("Boston", 5, 1),
            head("Labor Markets", 5, 3),
            head("New York", 6, 1),
            head("Labor Markets", 6, 3),
            // A continuation of nothing read before: the excerpt's start.
            head("Notes (continued)", 7, 2),
            // Templates sharing a title, each under its own label.
            head("IMPORTANT INFORMATION", 8, 1),
            at("doco:Paragraph", "Template 3-2", 9, None),
            head("IMPORTANT INFORMATION", 9, 1),
        ];
        read_continued_heads(&mut els);
        let kinds: Vec<(&str, &str)> = els
            .iter()
            .filter(|e| e.text != "Its text runs on.")
            .map(|e| (e.text.as_str(), e.kind.as_str()))
            .collect();
        let read_on = ["Box 1—continued", "Wiring Diagrams (continued)"];
        for (t, kind) in &kinds {
            let want = if read_on.contains(t) {
                "doco:Paragraph"
            } else {
                "doco:SectionTitle"
            };
            if *t != "PACKAGE OPTION ADDENDUM" && *t != "Template 3-2" {
                assert_eq!(*kind, want, "{t}");
            }
        }
        let addenda: Vec<&str> = kinds
            .iter()
            .filter(|(t, _)| *t == "PACKAGE OPTION ADDENDUM")
            .map(|(_, k)| *k)
            .collect();
        assert_eq!(addenda, ["doco:SectionTitle", "doco:Paragraph"]);
        assert_eq!(continued_from("Box 4—continued"), Some("Box 4"));
        assert_eq!(
            continued_from("Note 11 - Stock Compensation - Continued"),
            Some("Note 11 - Stock Compensation")
        );
        assert_eq!(continued_from("Continued Operations"), None);
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
    fn a_table_nested_in_anothers_cell_follows_it() {
        let content = vec![
            element("before", 10.0, 10.0, 300.0, 20.0, false),
            element("after", 10.0, 300.0, 300.0, 310.0, false),
        ];
        // A grid set in one of the outer table's cells, further down it.
        let tables = vec![
            element("nested", 120.0, 200.0, 290.0, 260.0, true),
            element("outer", 10.0, 40.0, 300.0, 280.0, true),
        ];
        let ordered = interleave_tables(content, tables);
        assert_eq!(
            ordered.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["before", "outer", "nested", "after"]
        );
    }

    #[test]
    fn a_wide_reading_inside_one_row_is_nested_and_one_across_rows_is_not() {
        let grid = |ys: Vec<f64>, y0: f64, y1: f64| Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 100.0, 300.0],
            bbox: BBox {
                x0: 0.0,
                y0,
                x1: 300.0,
                y1,
            },
            ys,
        };
        let ruled = grid(vec![0.0, 20.0, 40.0, 60.0, 140.0], 0.0, 140.0);
        // Nine columns of figures set in the last row's cell.
        let in_row = grid(vec![70.0, 90.0, 110.0, 130.0], 70.0, 130.0);
        assert!(nested_in_one_row(&ruled, &[&in_row]));
        // A table running down across the rows of a frame drawn round it.
        let across = grid(vec![25.0, 50.0, 75.0, 100.0], 25.0, 100.0);
        assert!(!nested_in_one_row(&ruled, &[&across]));
        // A box of three rows says too little.
        let small = grid(vec![0.0, 20.0, 60.0, 140.0], 0.0, 140.0);
        assert!(!nested_in_one_row(&small, &[&in_row]));
    }

    #[test]
    fn a_column_of_prose_beside_a_charts_scale_is_no_table() {
        // A row of words from `x`, each word a glyph, at 8 pt on the
        // baseline `y`.
        let row = |words: &str, x: f64, y: f64, glyphs: &mut Vec<Glyph>| {
            let mut at = x;
            for w in words.split_whitespace() {
                let width = 4.0 * w.chars().count() as f64;
                glyphs.push(Glyph {
                    text: w.into(),
                    bbox: Some(BBox {
                        x0: at,
                        y0: y - 8.0,
                        x1: at + width,
                        y1: y,
                    }),
                    page: 0,
                    origin: (at, y),
                    rotation_deg: 0.0,
                    font_size: 8.0,
                    weight: None,
                    italic: false,
                    advance: None,
                    draw_index: glyphs.len(),
                    ocr: None,
                });
                at += width + 3.0;
            }
        };
        let grid = Grid {
            layout: None,
            page: 0,
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 300.0,
                y1: 60.0,
            },
            xs: vec![0.0, 240.0, 300.0],
            ys: vec![0.0, 20.0, 40.0, 60.0],
        };
        // A tick of the axis at `y`, just right of the scale's numbers.
        let tick = |y: f64| crate::rule::Rule {
            bbox: BBox {
                x0: 272.0,
                y0: y - 0.2,
                x1: 277.0,
                y1: y + 0.2,
            },
            orientation: crate::rule::Orientation::Horizontal,
            page: 0,
        };
        let ticks: Vec<crate::rule::Rule> = [16.0, 36.0, 56.0].iter().map(|&y| tick(y)).collect();
        let scale = |glyphs: &mut Vec<Glyph>| {
            row("18.0", 252.0, 20.0, glyphs);
            row("17.5", 252.0, 40.0, glyphs);
            row("17.0", 252.0, 60.0, glyphs);
        };
        // The page's prose, read a line to a row beside the chart's scale.
        let mut prose = Vec::new();
        row(
            "spending have deteriorated. Business",
            0.0,
            20.0,
            &mut prose,
        );
        row(
            "investment rose moderately in the second half",
            0.0,
            40.0,
            &mut prose,
        );
        row(
            "of 2022, although new orders indexes, business",
            0.0,
            60.0,
            &mut prose,
        );
        scale(&mut prose);
        assert!(scale_beside_text(&grid, &prose, &[], &ticks));
        // With no tick by its numbers, it may be a table's column.
        assert!(!scale_beside_text(&grid, &prose, &[], &[]));
        // A chart's own labels beside its scale are no prose: kept, they
        // stay with the chart.
        let mut labels = Vec::new();
        row("Lower", 0.0, 20.0, &mut labels);
        row("Broadly similar", 0.0, 40.0, &mut labels);
        row("Higher", 0.0, 60.0, &mut labels);
        scale(&mut labels);
        assert!(!scale_beside_text(&grid, &labels, &[], &ticks));
    }

    #[test]
    fn a_ruled_grid_of_short_rows_reads_as_rows_and_a_frame_does_not() {
        let word = |x: f64, y: f64, i: usize| Glyph {
            text: "Value".into(),
            bbox: Some(BBox {
                x0: x,
                y0: y - 8.0,
                x1: x + 25.0,
                y1: y,
            }),
            page: 0,
            origin: (x, y),
            rotation_deg: 0.0,
            font_size: 8.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: i,
            ocr: None,
        };
        let grid = |xs: Vec<f64>, ys: Vec<f64>| Grid {
            layout: None,
            page: 0,
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: xs[xs.len() - 1],
                y1: ys[ys.len() - 1],
            },
            xs,
            ys,
        };
        // A short value in every cell of four rows.
        let rows = grid(
            vec![0.0, 100.0, 200.0, 300.0],
            vec![0.0, 20.0, 40.0, 60.0, 80.0],
        );
        let mut glyphs = Vec::new();
        for r in 0..4 {
            for c in 0..3 {
                glyphs.push(word(
                    c as f64 * 100.0 + 5.0,
                    r as f64 * 20.0 + 14.0,
                    glyphs.len(),
                ));
            }
        }
        assert!(reads_as_rows(&rows, &glyphs));
        // A frame round a page of prose: one cell of thirty words.
        let frame = grid(vec![0.0, 100.0, 300.0], vec![0.0, 20.0, 220.0, 240.0]);
        let prose: Vec<Glyph> = (0..30)
            .map(|i| {
                word(
                    105.0 + (i % 3) as f64 * 60.0,
                    34.0 + (i / 3) as f64 * 12.0,
                    i,
                )
            })
            .collect();
        assert!(!reads_as_rows(&frame, &prose));
    }

    fn head(text: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> Element {
        Element {
            level: Some(3),
            kind: "doco:SectionTitle".into(),
            ..element(text, x0, y0, x1, y1, false)
        }
    }

    fn texts(elements: &[Element]) -> Vec<&str> {
        elements.iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn a_side_head_level_with_a_tables_top_heads_it() {
        // Side heads in a column of their own, each set level with the top
        // of what it heads; the main column's last paragraph above.
        let content = vec![
            element("the last paragraph", 210.0, 402.0, 560.0, 417.0, false),
            head("RESULTS", 128.0, 436.0, 176.0, 442.5),
            element("small label", 131.0, 447.0, 176.0, 453.0, false),
        ];
        let tables = vec![element("results table", 210.0, 436.0, 560.0, 610.0, true)];
        assert_eq!(
            texts(&interleave_tables(content, tables)),
            vec![
                "the last paragraph",
                "RESULTS",
                "results table",
                "small label"
            ]
        );
    }

    #[test]
    fn labels_down_a_tables_side_are_its_rows_not_its_heads() {
        // Bold labels down the left of a table, one beside each row, its
        // label column not read as the table's: the head over them all
        // heads the table, not the first row's label.
        let content = vec![
            element("a statement", 190.0, 180.0, 470.0, 214.0, false),
            head("Outcomes", 62.0, 262.0, 150.0, 272.0),
            head("Theory", 62.0, 291.0, 112.0, 301.0),
            head("Practice", 62.0, 377.0, 106.0, 386.0),
            head("Values", 62.0, 452.0, 101.0, 461.0),
        ];
        let tables = vec![element("outcomes table", 188.0, 284.0, 495.0, 515.0, true)];
        assert_eq!(
            texts(&interleave_tables(content, tables)),
            vec![
                "a statement",
                "Outcomes",
                "outcomes table",
                "Theory",
                "Practice",
                "Values"
            ]
        );
    }

    #[test]
    fn two_tables_side_by_side_read_each_over_its_note() {
        // Under one heading, a table at the left and one at the right, each
        // titled in its grid and noted beneath; the right column's last
        // section ends above them.
        let content = vec![
            element("right column text", 330.0, 402.0, 575.0, 470.0, false),
            head("Examples", 44.0, 506.0, 140.0, 515.0),
            element("left note", 46.0, 603.0, 250.0, 609.0, false),
            element("right note", 333.0, 603.0, 560.0, 609.0, false),
        ];
        for (left_top, right_top) in [(520.0, 520.0), (520.4, 519.8)] {
            let tables = vec![
                element("left table", 44.0, left_top, 290.0, 598.0, true),
                element("right table", 330.0, right_top, 575.0, 598.0, true),
            ];
            assert_eq!(
                texts(&interleave_tables(content.clone(), tables)),
                vec![
                    "right column text",
                    "Examples",
                    "left table",
                    "left note",
                    "right table",
                    "right note"
                ]
            );
        }
    }

    #[test]
    fn a_table_follows_the_heading_set_beside_it() {
        // A catalogue: each product's heading at the left over its photo,
        // its table to the right, level with the heading. The first
        // product's table runs a little under the second's.
        let heading = |text: &str, y0: f64| Element {
            level: Some(4),
            ..Element {
                kind: "doco:SectionTitle".into(),
                ..element(text, 30.0, y0, 200.0, y0 + 9.0, false)
            }
        };
        let content = vec![
            heading("Sill Gasket", 30.0),
            element(
                "Sill Gasket seals the gap",
                30.0,
                429.0,
                290.0,
                445.0,
                false,
            ),
            heading("Framing Gasket", 507.0),
            element("UPS option", 30.0, 512.0, 68.0, 520.0, false),
            element(
                "Framing Gasket seals the gap",
                361.0,
                675.0,
                572.0,
                691.0,
                false,
            ),
        ];
        let tables = vec![
            element("sill table", 30.0, 59.0, 369.0, 404.0, true),
            element("framing table", 360.0, 520.0, 569.0, 664.0, true),
        ];
        let ordered = interleave_tables(content, tables);
        assert_eq!(
            ordered.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec![
                "Sill Gasket",
                "sill table",
                "Sill Gasket seals the gap",
                "Framing Gasket",
                "UPS option",
                "framing table",
                "Framing Gasket seals the gap",
            ]
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
        assert!(heading_yields_to_figure(Evidence::Bold, true, false));
        assert!(heading_yields_to_figure(Evidence::FontSize, true, false));
        assert!(!heading_yields_to_figure(Evidence::Numbering, true, false));
        assert!(!heading_yields_to_figure(Evidence::Sequence, true, false));
        assert!(!heading_yields_to_figure(Evidence::Title, true, true));
        assert!(!heading_yields_to_figure(Evidence::Outline, true, true));
        assert!(!heading_yields_to_figure(Evidence::Bold, false, false));
    }

    #[test]
    fn a_number_inside_a_drawing_is_a_callout() {
        assert!(heading_yields_to_figure(Evidence::Numbering, true, true));
        assert!(heading_yields_to_figure(Evidence::Sequence, true, true));
    }

    #[test]
    fn a_grid_inside_line_art_is_the_drawings() {
        use crate::rule::{Orientation, Rule};
        // A 3 x 3 grid, 90pt square.
        let grid = Grid {
            layout: None,
            page: 0,
            xs: vec![0.0, 30.0, 60.0, 90.0],
            ys: vec![0.0, 30.0, 60.0, 90.0],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 90.0,
                y1: 90.0,
            },
        };
        let rule = |x0: f64, y0: f64, x1: f64, y1: f64, o: Orientation| Rule {
            bbox: BBox { x0, y0, x1, y1 },
            orientation: o,
            page: 0,
        };
        // A table: its four boundaries each way, each drawn cell by cell.
        let mut table = Vec::new();
        for k in 0..4 {
            let at = 30.0 * k as f64;
            for c in 0..3 {
                let from = 30.0 * c as f64;
                table.push(rule(at, from, at, from + 30.0, Orientation::Vertical));
                table.push(rule(from, at, from + 30.0, at, Orientation::Horizontal));
            }
        }
        assert!(!line_art(&grid, &table));
        // A unit's front panel: a terminal's outline every 4pt each way.
        let mut panel = table.clone();
        for k in 0..20 {
            let at = 2.0 + 4.0 * k as f64;
            panel.push(rule(at, 40.0, at, 46.0, Orientation::Vertical));
            panel.push(rule(40.0, at, 46.0, at, Orientation::Horizontal));
        }
        assert!(line_art(&grid, &panel));
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
            italic: false,
            advance: None,
            draw_index,
            ocr: None,
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
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        };
        assert!(prefer_aligned_over_ruled(&[ruled], &[aligned], &[glyph]));
    }

    fn sized(text: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> Line {
        Line {
            text: text.into(),
            bbox: BBox { x0, y0, x1, y1 },
            ..line(x0, y0)
        }
    }

    #[test]
    fn a_charts_labels_are_no_text_picture() {
        // Ticks and a legend, short and scattered over a 300 × 200 chart:
        // its lettering, read as the figure's.
        let chart = BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 300.0,
            y1: 200.0,
        };
        let labels: Vec<Line> = (0..6)
            .map(|i| {
                sized(
                    "40",
                    5.0,
                    20.0 + 30.0 * i as f64,
                    20.0,
                    28.0 + 30.0 * i as f64,
                )
            })
            .chain([sized("Share Taxi", 220.0, 10.0, 280.0, 18.0)])
            .collect();
        assert!(!text_picture(&chart, &labels));
    }

    #[test]
    fn lines_across_a_picture_are_text_set_as_an_image() {
        // A footnote set as an image: its lines run across the picture.
        let strip = BBox {
            x0: 46.0,
            y0: 479.0,
            x1: 573.0,
            y1: 530.0,
        };
        let lines = vec![
            sized(
                "1 Evaluated against 100 internal test queries",
                52.0,
                485.0,
                575.0,
                494.0,
            ),
            sized("amount of returned information", 52.0, 499.0, 278.0, 507.0),
            sized(
                "2 State-of-the-art, current highest level",
                52.0,
                514.0,
                324.0,
                523.0,
            ),
        ];
        assert!(text_picture(&strip, &lines));
    }

    fn own(x0: f64, x1: f64, y0: f64, size: f32) -> Glyph {
        Glyph {
            text: "y".into(),
            bbox: Some(BBox {
                x0,
                y0,
                x1,
                y1: y0 + f64::from(size) * 0.7,
            }),
            page: 0,
            origin: (x0, y0 + f64::from(size) * 0.7),
            rotation_deg: 0.0,
            font_size: size,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
            ocr: None,
        }
    }

    #[test]
    fn a_wordmark_among_a_charts_labels_is_the_pages_text() {
        // A word set as an image, 7.5pt high, a little over an em from the
        // 5.3pt label before it on its row.
        let mark = BBox {
            x0: 138.0,
            y0: 307.5,
            x1: 162.0,
            y1: 315.0,
        };
        assert!(inline_image(&mark, &[own(127.8, 130.3, 309.9, 5.3)]));
        // Two lines' height and more is no word in a line.
        let banner = BBox { y1: 330.0, ..mark };
        assert!(!inline_image(&banner, &[own(127.8, 130.3, 309.9, 5.3)]));
        // Nor is a word with no text of the page's on its row near it.
        assert!(!inline_image(&mark, &[own(40.0, 43.0, 309.9, 5.3)]));
    }
}
