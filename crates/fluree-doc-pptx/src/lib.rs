//! PPTX (OOXML) to DoCO-typed document elements.
//!
//! A deck is the one structural format with a real page concept: each slide
//! is a page, so `page` carries the slide index and the DoCO graph keeps the
//! deck's pagination. Geometry is still absent — shapes have positions in
//! EMUs, but they describe a canvas layout rather than a text flow's box, and
//! reporting them as `bbox` would invite consumers to treat a deck like a
//! scanned page.
//!
//! The mapping is declared, not inferred: a shape whose placeholder type is
//! `title` or `ctrTitle` is the slide's heading, `a:tbl` is a real table with
//! `gridSpan` / `rowSpan` merges, and paragraphs with a bullet character or a
//! non-zero outline level are list items.
//!
//! Slides are read in the deck's order: the slide list in
//! `ppt/presentation.xml`, each entry resolved through the presentation's
//! relationships to its part. A slide moved in PowerPoint keeps its part's
//! name, so `slide2.xml` can come first. A package with no usable list is
//! read in the numeric order of its slide parts' names.
//!
//! **Charts carry their data.** A chart in a deck is not ink: `c:cat` and
//! `c:val` hold the categories and values outright, cached beside the
//! formula that produced them. So a pie of employees by region yields
//! `EMEA -> 28.0` as a fact, where the same chart printed to PDF is vector
//! strokes from which the pairing cannot be recovered. Each chart becomes a
//! table — categories down, one column per series — so its numbers are
//! queryable like any other table's.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_ooxml::{attr, local, Package, Relations};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub enum PptxError {
    Zip(String),
    Xml(String),
    NoSlides,
    /// Past what one document may make ([`Budget`]), naming it.
    TooLarge(TooLarge),
}

impl std::fmt::Display for PptxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .pptx: {e}"),
            Self::Xml(e) => write!(f, "malformed slide XML: {e}"),
            Self::NoSlides => write!(f, "package has no slides"),
            Self::TooLarge(e) => write!(f, "PPTX deck too large: {e}"),
        }
    }
}

/// A bound passed is the error's source: [`TooLarge::limit`] says which.
impl std::error::Error for PptxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge(e) => Some(e),
            _ => None,
        }
    }
}

impl From<TooLarge> for PptxError {
    fn from(e: TooLarge) -> Self {
        Self::TooLarge(e)
    }
}

/// Parse a `.pptx` file's bytes into elements, in slide order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, PptxError> {
    parse_with_info(bytes).map(|(elements, _)| elements)
}

/// [`parse`], with what the file declares about itself in its core
/// properties, read from the one opening of the package.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), PptxError> {
    let mut pkg = Package::open(bytes).map_err(PptxError::Zip)?;
    // What the deck makes, slide by slide, is spent from one budget as it
    // is made, and each part it is read from, inflated whole, before: a
    // slide listed a thousand times is read a thousand times, and its
    // relationships once.
    let mut budget = Budget::default();
    let slides = slide_parts(&mut pkg, &mut budget)?;
    if slides.is_empty() {
        return Err(PptxError::NoSlides);
    }

    let mut out = Vec::new();
    // Each chart part read once, by its name, however many slides or
    // frames name it: `None` where it holds no series.
    let mut parsed: HashMap<String, Option<ChartData>> = HashMap::new();
    // Each slide's charts by relationship id, the first of an id that
    // names a chart with series, resolved once for its part however many
    // times the deck lists it, and spent once: read again for each
    // listing, a slide of 300,000 chart relationships listed 100 times
    // took 17 s, and the budget allows a million listings.
    let mut slide_charts: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (idx, name) in slides.iter().enumerate() {
        let Some(xml) = pkg.read_within(name, &mut budget)? else {
            continue;
        };
        if !slide_charts.contains_key(name) {
            // Charts live in their own parts; the slide only names a
            // relationship id, so resolve those before walking it.
            let mut by_id: HashMap<String, String> = HashMap::new();
            for r in pkg.relationships(name) {
                if !r.is("chart") || r.external || by_id.contains_key(&r.id) {
                    continue;
                }
                if !parsed.contains_key(&r.target) {
                    let chart = match pkg.read_within(&r.target, &mut budget)? {
                        Some(cx) => parse_chart(&cx, &mut budget)?,
                        None => None,
                    };
                    parsed.insert(r.target.clone(), chart);
                }
                if parsed[&r.target].is_some() {
                    budget.spend_text(&r.id)?;
                    budget.spend_text(&r.target)?;
                    by_id.insert(r.id, r.target);
                }
            }
            slide_charts.insert(name.clone(), by_id);
        }
        let by_id = &slide_charts[name];
        let chart = |id: &str| parsed.get(by_id.get(id)?)?.as_ref();
        out.extend(parse_slide(&xml, idx, &chart, &mut budget)?);
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok((out, pkg.info()))
}

/// The deck's slide parts, in its order.
///
/// The presentation part lists the slides as relationship ids
/// (`<p:sldId r:id="rId2"/>`), and its relationships name each one's part.
/// A package without that list, or whose list names no part it holds, is
/// read in the numeric order of its slide parts' names, where `slide10`
/// comes after `slide2`.
fn slide_parts(pkg: &mut Package<'_>, budget: &mut Budget) -> Result<Vec<String>, TooLarge> {
    let presentation = pkg.main_part("ppt/presentation.xml");
    let rels = pkg.relationships(&presentation);
    let names = pkg.part_names();
    // Each slide listed found in one lookup among the relationships, and
    // its part in one among the package's: a scan of each for every
    // listing made the listing's cost their product.
    let relations = Relations::new(&rels);
    let held: HashSet<&str> = names.iter().map(String::as_str).collect();
    let listed: Vec<String> = pkg
        .read_within(&presentation, budget)?
        .map(|xml| slide_ids(&xml))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| relations.part(&id))
        .filter(|p| held.contains(p))
        .map(str::to_string)
        .collect();
    if !listed.is_empty() {
        return Ok(listed);
    }
    let mut numbered: Vec<(usize, String)> = names
        .into_iter()
        .filter_map(|n| slide_number(&n).map(|k| (k, n)))
        .collect();
    numbered.sort();
    Ok(numbered.into_iter().map(|(_, n)| n).collect())
}

/// The relationship ids of `presentation.xml`'s slide list, in order.
///
/// A `p:sldId` carries two ids: its own number, `id`, and the relationship
/// id under the relationships namespace, `r:id`. Only the prefixed one names
/// a part.
fn slide_ids(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == "sldId" => {
                let rel = fluree_doc_ooxml::attributes(&e).find(|a| {
                    let key = a.key.as_ref();
                    key.contains(&b':') && local(key) == "id"
                });
                if let Some(a) = rel {
                    out.push(String::from_utf8_lossy(&a.value).into_owned());
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A named series and the values it plots.
pub type Series = (String, Vec<String>);

/// A chart's declared data: title, category labels, and one series per
/// plotted column.
pub type ChartData = (String, Vec<String>, Vec<Series>);

/// One chart's declared data: a title, the category labels, and one named
/// series of values per plotted column.
#[derive(Default)]
struct Chart {
    title: String,
    categories: Vec<String>,
    series: Vec<Series>,
}

/// What the file declares about itself in its core properties: title,
/// author, and when it was made and last saved. Empty where there are none
/// or the file is not a package.
pub fn info(bytes: &[u8]) -> DocumentInfo {
    fluree_doc_ooxml::info(bytes)
}

/// Most points a chart's cache is read to: a `ptCount` or an `idx` beyond
/// it is a malformed part, not a chart anyone plotted.
const MAX_POINTS: usize = 100_000;

/// Read a chart part, within a budget of its own. Values come from the cached `c:strCache`/`c:numCache`
/// blocks, which hold what the chart actually plots — the `c:f` formula
/// beside them points into a workbook that may not travel with the deck.
///
/// A cache states how many points it has (`c:ptCount`) and each point's
/// place (`c:pt idx`), and leaves out the ones with no value: a bar not
/// drawn is a point not written. So each value goes to its index, and a
/// series' gaps stay gaps instead of moving its later values onto earlier
/// categories. Of a multi-level category axis, the first level is read: the
/// labels next to the plot, one per point.
pub fn parse_chart_xml(xml: &str) -> Result<Option<ChartData>, PptxError> {
    Ok(parse_chart(xml, &mut Budget::default())?)
}

/// [`parse_chart_xml`], what it makes spent from the deck's `budget` before
/// it is made: each point read, and each series' or category list's
/// values laid out by index, as many as its cache declares.
fn parse_chart(xml: &str, budget: &mut Budget) -> Result<Option<ChartData>, TooLarge> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut chart = Chart::default();

    // Which part of a series the current cache belongs to.
    #[derive(PartialEq, Clone, Copy)]
    enum Slot {
        None,
        Name,
        Cat,
        Val,
    }
    let mut slot = Slot::None;
    let mut in_title = false;
    let mut in_v = false;
    // The cache being read: its declared size, the point being read, how
    // many category levels have opened, and its values by index.
    let mut count = 0usize;
    let mut point: Option<usize> = None;
    let mut level = 0usize;
    let mut pending: Vec<(usize, String)> = Vec::new();
    let mut series_name = String::new();
    let mut text = String::new();
    let mut depth_title = 0usize;

    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => match local(e.name().as_ref()) {
                "title" => {
                    in_title = true;
                    depth_title = 0;
                }
                "ser" => {
                    pending.clear();
                    series_name.clear();
                }
                "ptCount" => {
                    count = attr(&e, "val")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(MAX_POINTS);
                }
                "pt" => point = attr(&e, "idx").and_then(|v| v.parse().ok()),
                "lvl" => level += 1,
                "tx" => slot = Slot::Name,
                "cat" | "xVal" => slot = Slot::Cat,
                "val" | "yVal" => slot = Slot::Val,
                "v" => in_v = true,
                "t" if in_title => in_v = true,
                _ => {}
            },
            Event::Text(t) => {
                if in_v {
                    text.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Event::End(e) => match local(e.name().as_ref()) {
                "v" | "t" => {
                    if in_v {
                        let val = std::mem::take(&mut text);
                        if in_title && depth_title == 0 {
                            if !chart.title.is_empty() {
                                chart.title.push(' ');
                            }
                            chart.title.push_str(val.trim());
                        } else {
                            match slot {
                                Slot::Name => series_name = val.trim().to_string(),
                                Slot::Cat | Slot::Val if level <= 1 => {
                                    // A point with no index follows the
                                    // one before it.
                                    let at = point.take().unwrap_or_else(|| {
                                        pending.last().map_or(0, |(i, _)| i + 1)
                                    });
                                    if at < MAX_POINTS {
                                        let val = val.trim();
                                        budget.spend_cells(1)?;
                                        budget.spend_text(val)?;
                                        pending.push((at, val.to_string()));
                                    }
                                }
                                Slot::Cat | Slot::Val | Slot::None => {}
                            }
                        }
                        in_v = false;
                    }
                }
                "title" => in_title = false,
                "pt" => point = None,
                "cat" | "xVal" => {
                    let points = by_index(&mut pending, &mut count, budget)?;
                    if chart.categories.is_empty() {
                        chart.categories = points;
                    }
                    slot = Slot::None;
                    level = 0;
                }
                "val" | "yVal" => {
                    let points = by_index(&mut pending, &mut count, budget)?;
                    chart
                        .series
                        .push((std::mem::take(&mut series_name), points));
                    slot = Slot::None;
                }
                "tx" => {
                    pending.clear();
                    count = 0;
                    slot = Slot::None;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok((!chart.series.is_empty()).then_some((chart.title, chart.categories, chart.series)))
}

/// A cache's values laid out by index: as many as it declares or holds,
/// blank where it wrote no point, spent from `budget` before they are made
/// (a series with none still one slot). Leaves the cache state empty for
/// the next.
fn by_index(
    points: &mut Vec<(usize, String)>,
    count: &mut usize,
    budget: &mut Budget,
) -> Result<Vec<String>, TooLarge> {
    let n = points
        .iter()
        .map(|(i, _)| i + 1)
        .max()
        .unwrap_or(0)
        .max(*count);
    budget.spend_cells(n.max(1) as u64)?;
    let mut out = vec![String::new(); n];
    for (i, v) in points.drain(..) {
        out[i] = v;
    }
    *count = 0;
    Ok(out)
}

/// A chart as a table: categories down the first column, one column per
/// series. The numbers become cells, so a consumer can ask what EMEA's share
/// was without looking at a picture.
///
/// Its grid, a row for each category or value of the longest series and a
/// column for each series, is spent from `budget` before it is made, and
/// its cells that hold text and their text after: a chart its frames name
/// many times is read once and made once for each.
fn chart_element(
    (title, categories, series): &ChartData,
    page: usize,
    budget: &mut Budget,
) -> Result<Vec<Element>, TooLarge> {
    let mut out = Vec::new();
    if !title.is_empty() {
        keep(
            &mut out,
            element("doco:SectionTitle", title.clone(), Some(2), page),
            budget,
        )?;
    }
    let n = categories
        .len()
        .max(series.iter().map(|(_, v)| v.len()).max().unwrap_or(0));
    let width = 1 + series.len();
    budget.spend_cells((n as u64 + 1).saturating_mul(width as u64))?;
    let mut header = vec![String::new()];
    for (name, _) in series {
        header.push(name.clone());
    }
    let mut rows = vec![header];
    for i in 0..n {
        let mut row = vec![categories.get(i).cloned().unwrap_or_default()];
        for (_, vals) in series {
            row.push(vals.get(i).cloned().unwrap_or_default());
        }
        rows.push(row);
    }
    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None, page);
    e.header_rows = Some(1);
    e.cells = Some(rows);
    e.evidence = "pptx-chart";
    spend_table(&e, budget)?;
    out.push(e);
    Ok(out)
}

/// `ppt/slides/slide12.xml` → 12. Notes and layouts are not slides.
fn slide_number(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("ppt/slides/slide")?;
    rest.strip_suffix(".xml")?.parse().ok()
}

#[derive(Default)]
struct Cell {
    text: String,
    span: usize,
    h_continue: bool,
    v_continue: bool,
}

/// Parse one slide's XML into elements tagged with `page`.
pub fn parse_slide_xml(xml: &str, page: usize) -> Result<Vec<Element>, PptxError> {
    parse_slide_with_charts(xml, page, &[])
}

/// As [`parse_slide_xml`], with the slide's chart parts resolved so each
/// chart is emitted where its frame sits in reading order.
pub fn parse_slide_with_charts(
    xml: &str,
    page: usize,
    charts: &[(String, String)],
) -> Result<Vec<Element>, PptxError> {
    let mut budget = Budget::default();
    let mut parsed = Vec::new();
    for (id, cx) in charts {
        if let Some(chart) = parse_chart(cx, &mut budget)? {
            parsed.push((id.clone(), chart));
        }
    }
    // The first of an id, as a scan finds it.
    let mut by_id: HashMap<&str, &ChartData> = HashMap::new();
    for (id, chart) in &parsed {
        by_id.entry(id.as_str()).or_insert(chart);
    }
    parse_slide(xml, page, &|id| by_id.get(id).copied(), &mut budget)
}

/// [`parse_slide_with_charts`], what the slide makes spent from the deck's
/// `budget` as it is made.
///
/// `chart` finds the chart a frame names by relationship id in one lookup:
/// a scan of the slide's charts for each frame made the slide's cost their
/// product.
fn parse_slide<'c>(
    xml: &str,
    page: usize,
    chart: &dyn Fn(&str) -> Option<&'c ChartData>,
    budget: &mut Budget,
) -> Result<Vec<Element>, PptxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<Element> = Vec::new();

    let mut text = String::new();
    let mut in_text = false;
    // Shape state: a placeholder's type decides whether its text is a title.
    let mut shape = Shape::Body;
    let mut para_level = 0usize;
    let mut para_bullet = false;
    let mut in_table = 0usize;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut cell = Cell::default();
    let mut in_cell = false;

    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(PptxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "t" => in_text = true,
                "br" => push(&mut text, &mut cell, in_cell, " "),
                "ph" => {
                    if let Some(t) = attr(&e, "type") {
                        shape = match t.as_str() {
                            // The title slide's title is the deck's.
                            "ctrTitle" if page == 0 => Shape::DeckTitle,
                            "subTitle" if page == 0 => Shape::DeckSubtitle,
                            "title" | "ctrTitle" => Shape::SlideTitle,
                            _ => Shape::Body,
                        };
                    }
                }
                "pPr" => {
                    para_level = attr(&e, "lvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                "buChar" | "buAutoNum" => para_bullet = true,
                "buNone" => para_bullet = false,
                "tbl" => {
                    in_table += 1;
                    if in_table == 1 {
                        rows.clear();
                    }
                }
                "chart" => {
                    // The frame names its part by relationship id.
                    if let Some(id) = attr(&e, "id") {
                        if let Some(chart) = chart(&id) {
                            out.extend(chart_element(chart, page, budget)?);
                        }
                    }
                }
                "tc" => {
                    in_cell = true;
                    cell = Cell {
                        span: attr(&e, "gridSpan")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1),
                        h_continue: attr(&e, "hMerge").is_some(),
                        v_continue: attr(&e, "vMerge").is_some(),
                        ..Default::default()
                    };
                }
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_text {
                    let s = t.unescape().unwrap_or_default().to_string();
                    push(&mut text, &mut cell, in_cell, &s);
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "t" => in_text = false,
                "p" => {
                    if in_cell {
                        if !cell.text.is_empty() && !cell.text.ends_with(' ') {
                            cell.text.push(' ');
                        }
                    } else {
                        flush(
                            &mut out,
                            &mut text,
                            (
                                page,
                                shape,
                                (para_bullet || para_level > 0).then_some(para_level),
                            ),
                            budget,
                        )?;
                        para_bullet = false;
                        para_level = 0;
                    }
                }
                "tc" => {
                    in_cell = false;
                    row.push(std::mem::take(&mut cell));
                }
                "tr" => rows.push(std::mem::take(&mut row)),
                "tbl" => {
                    in_table = in_table.saturating_sub(1);
                    if in_table == 0 {
                        emit_table(&mut out, std::mem::take(&mut rows), page, budget)?;
                    }
                }
                "sp" => {
                    // A shape's title-ness does not leak into the next shape.
                    flush(&mut out, &mut text, (page, shape, None), budget)?;
                    shape = Shape::Body;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    flush(&mut out, &mut text, (page, Shape::Body, None), budget)?;
    // A subtitle follows its title, whichever shape the slide lists first:
    // the subtitle and what the slide lists after it, up to the title, go
    // after the title. With no title it subtitles nothing, and is text.
    match (
        out.iter().position(|e| e.kind == "doco:Subtitle"),
        out.iter().position(|e| e.kind == "doco:Title"),
    ) {
        (Some(s), Some(t)) if s < t => {
            let run: Vec<Element> = out.drain(s..t).collect();
            out.splice(s + 1..s + 1, run);
        }
        (Some(s), None) => out[s].kind = "doco:Paragraph".into(),
        _ => {}
    }
    Ok(out)
}

fn push(text: &mut String, cell: &mut Cell, in_cell: bool, s: &str) {
    if in_cell {
        cell.text.push_str(s);
    } else {
        text.push_str(s);
    }
}

fn element(kind: &str, text: String, level: Option<usize>, page: usize) -> Element {
    Element {
        page,
        level,
        ..Element::new(kind, text, "pptx", "pptx")
    }
}

/// What a shape's text is, by its placeholder type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Body,
    /// A slide's title: the deck's section heading.
    SlideTitle,
    /// The title on the deck's title slide: the deck's own.
    DeckTitle,
    /// The subtitle on the deck's title slide: the deck's subtitle, its
    /// first paragraph. The ones after it (a presenter, a date under the
    /// subtitle) are text.
    DeckSubtitle,
}

/// `e` kept, it and its text spent from `budget` first.
fn keep(out: &mut Vec<Element>, e: Element, budget: &mut Budget) -> Result<(), TooLarge> {
    budget.spend_elements(1)?;
    budget.spend_text(&e.text)?;
    out.push(e);
    Ok(())
}

/// A table's element, its cells that hold text and their text spent from
/// `budget` (its grid was, before it was made), and what DoCO copies of
/// its cells.
fn spend_table(e: &Element, budget: &mut Budget) -> Result<(), TooLarge> {
    budget.spend_elements(1)?;
    budget.spend_cell_texts(e.cells.iter().flatten().flatten().map(String::as_str))?;
    budget.spend_copies(e)
}

/// The text gathered in `buf` as an element of the slide `page`, by the
/// shape it is in. `item` is a list item's depth, its paragraph's outline
/// level `lvl`, for a paragraph read as one.
fn flush(
    out: &mut Vec<Element>,
    buf: &mut String,
    (page, shape, item): (usize, Shape, Option<usize>),
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    let t = std::mem::take(buf)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() {
        return Ok(());
    }
    let e = if shape == Shape::DeckTitle {
        element("doco:Title", t, None, page)
    } else if shape == Shape::DeckSubtitle && !out.iter().any(|e| e.kind == "doco:Subtitle") {
        element("doco:Subtitle", t, None, page)
    } else if shape == Shape::SlideTitle {
        element("doco:SectionTitle", t, Some(1), page)
    } else if let Some(depth) = item {
        Element {
            depth,
            ..element("doc:ListItem", t, None, page)
        }
    } else {
        element("doco:Paragraph", t, None, page)
    };
    keep(out, e, budget)
}

/// A table's rows as a `doco:Table`, its grid (its rows padded to its
/// widest) spent from `budget` before it is made.
fn emit_table(
    out: &mut Vec<Element>,
    rows: Vec<Vec<Cell>>,
    page: usize,
    budget: &mut Budget,
) -> Result<(), TooLarge> {
    let rows: Vec<Vec<Cell>> = rows.into_iter().filter(|r| !r.is_empty()).collect();
    if rows.is_empty() {
        return Ok(());
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if width == 0 {
        return Ok(());
    }
    let n = rows.len();
    budget.spend_cells((n as u64).saturating_mul(width as u64))?;
    let mut grid = vec![String::new(); n * width];
    let mut m_left = vec![false; n * width];
    let mut m_down = vec![false; n * width];
    for (r, cells) in rows.iter().enumerate() {
        for (c, cell) in cells.iter().enumerate().take(width) {
            grid[r * width + c] = cell.text.split_whitespace().collect::<Vec<_>>().join(" ");
            // PowerPoint emits every grid cell and marks continuations, so
            // the flags come straight off the cell rather than from spans.
            if cell.h_continue && c > 0 {
                m_left[r * width + c] = true;
            }
            if cell.v_continue && r > 0 {
                m_down[r * width + c] = true;
            }
            // A span is cut at the grid's edge.
            for k in 1..cell.span.max(1).min(width - c) {
                m_left[r * width + c + k] = true;
            }
        }
    }
    let cells: Vec<Vec<String>> = (0..n)
        .map(|r| grid[r * width..(r + 1) * width].to_vec())
        .collect();
    let text = cells
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = element("doco:Table", text, None, page);
    e.header_rows = Some(1.min(n));
    e.cells = Some(cells);
    e.merged_left = m_left.iter().any(|x| *x).then_some(m_left);
    e.merged_down = m_down.iter().any(|x| *x).then_some(m_down);
    spend_table(&e, budget)?;
    out.push(e);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::scaling::assert_linear;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn slide(body: &str) -> String {
        format!("<p:sld {NS}><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>")
    }

    fn shape(ph: Option<&str>, paras: &str) -> String {
        let nv = ph
            .map(|t| format!("<p:nvSpPr><p:nvPr><p:ph type=\"{t}\"/></p:nvPr></p:nvSpPr>"))
            .unwrap_or_default();
        format!("<p:sp>{nv}<p:txBody>{paras}</p:txBody></p:sp>")
    }

    fn para(text: &str) -> String {
        format!("<a:p><a:r><a:t>{text}</a:t></a:r></a:p>")
    }

    #[test]
    fn the_title_slides_title_is_the_decks() {
        let first = slide(&format!(
            "{}{}",
            shape(Some("ctrTitle"), &para("Quarterly Review")),
            shape(Some("subTitle"), &para("Q3 2026"))
        ));
        let els = parse_slide_xml(&first, 0).unwrap();
        assert_eq!(els[0].kind, "doco:Title");
        assert_eq!(els[1].kind, "doco:Subtitle");
        // Listed before the title, the subtitle still follows it, and the
        // subtitle's next paragraph is text.
        let first = slide(&format!(
            "{}{}",
            shape(
                Some("subTitle"),
                &format!("{}{}", para("Q3 2026"), para("Ada Park"))
            ),
            shape(Some("ctrTitle"), &para("Quarterly Review")),
        ));
        let kinds: Vec<(String, String)> = parse_slide_xml(&first, 0)
            .unwrap()
            .into_iter()
            .map(|e| (e.kind, e.text))
            .collect();
        assert_eq!(
            kinds,
            [
                ("doco:Title".to_string(), "Quarterly Review".to_string()),
                ("doco:Subtitle".to_string(), "Q3 2026".to_string()),
                ("doco:Paragraph".to_string(), "Ada Park".to_string()),
            ]
        );
        // A later title slide, a section divider, is a heading like any
        // slide's title.
        let later = slide(&shape(Some("ctrTitle"), &para("Appendix")));
        assert_eq!(
            parse_slide_xml(&later, 7).unwrap()[0].kind,
            "doco:SectionTitle"
        );
    }

    #[test]
    fn slide_numbers_order_the_deck_not_the_archive() {
        assert_eq!(slide_number("ppt/slides/slide10.xml"), Some(10));
        assert_eq!(slide_number("ppt/slides/slide2.xml"), Some(2));
        assert_eq!(slide_number("ppt/slideLayouts/slideLayout2.xml"), None);
        assert_eq!(slide_number("ppt/notesSlides/notesSlide1.xml"), None);
        let mut v = [("slide10", 10), ("slide2", 2)];
        v.sort_by_key(|(_, n)| *n);
        assert_eq!(v[0].1, 2, "slide10 must not sort before slide2");
    }

    #[test]
    fn a_title_placeholder_is_the_slides_heading() {
        let x = slide(&format!(
            "{}{}",
            shape(Some("title"), &para("Quarterly Review")),
            shape(None, &para("Revenue grew."))
        ));
        let els = parse_slide_xml(&x, 3).unwrap();
        assert_eq!(els[0].kind, "doco:SectionTitle");
        assert_eq!(els[0].text, "Quarterly Review");
        assert_eq!(els[1].kind, "doco:Paragraph");
        assert!(els.iter().all(|e| e.page == 3), "slide index is the page");
    }

    #[test]
    fn bulleted_and_outlined_paragraphs_are_list_items() {
        let x = slide(&shape(
            None,
            "<a:p><a:pPr><a:buChar a:char=\"•\"/></a:pPr><a:r><a:t>bullet</a:t></a:r></a:p>\
             <a:p><a:pPr lvl=\"1\"/><a:r><a:t>indented</a:t></a:r></a:p>",
        ));
        let els = parse_slide_xml(&x, 0).unwrap();
        assert_eq!(els[0].kind, "doc:ListItem");
        assert_eq!(els[1].kind, "doc:ListItem");
        // An outline level is a list item's depth.
        assert_eq!((els[0].depth, els[1].depth), (0, 1));
    }

    #[test]
    fn tables_carry_declared_merges() {
        let x = slide(&format!(
            "<p:graphicFrame><a:graphic><a:graphicData><a:tbl>\
               <a:tr><a:tc><a:txBody>{}</a:txBody></a:tc>\
                     <a:tc hMerge=\"1\"><a:txBody>{}</a:txBody></a:tc></a:tr>\
               <a:tr><a:tc><a:txBody>{}</a:txBody></a:tc>\
                     <a:tc><a:txBody>{}</a:txBody></a:tc></a:tr>\
             </a:tbl></a:graphicData></a:graphic></p:graphicFrame>",
            para("Banner"),
            para(""),
            para("A"),
            para("B")
        ));
        let els = parse_slide_xml(&x, 0).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        assert_eq!(t.cells.as_ref().unwrap()[1], vec!["A", "B"]);
        assert!(t.merged_left.as_ref().expect("hMerge recorded")[1]);
    }

    /// A chart part as PowerPoint writes it: the plotted values are cached
    /// beside the formula that produced them.
    fn chart_xml(series: &str) -> String {
        format!(
            "<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\">\
               <c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Employees by region</a:t></a:r></a:p></c:rich></c:tx></c:title>\
               <c:plotArea>{series}</c:plotArea></c:chart></c:chartSpace>"
        )
    }

    fn ser(name: &str, cats: &[&str], vals: &[&str]) -> String {
        let pts = |v: &[&str]| {
            v.iter()
                .enumerate()
                .map(|(i, x)| format!("<c:pt idx=\"{i}\"><c:v>{x}</c:v></c:pt>"))
                .collect::<String>()
        };
        format!(
            "<c:ser><c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:pt idx=\"0\"><c:v>{name}</c:v></c:pt></c:strCache></c:strRef></c:tx>\
             <c:cat><c:strRef><c:f>Sheet1!$A$2</c:f><c:strCache>{}</c:strCache></c:strRef></c:cat>\
             <c:val><c:numRef><c:f>Sheet1!$B$2</c:f><c:numCache>{}</c:numCache></c:numRef></c:val></c:ser>",
            pts(cats),
            pts(vals)
        )
    }

    #[test]
    fn a_chart_yields_its_categories_paired_with_values() {
        // The pairing a printed chart destroys: in a deck it is declared.
        let x = chart_xml(&ser(
            "Share",
            &["North America", "EMEA", "Asia Pacific"],
            &["34.5", "28.0", "17.5"],
        ));
        let chart = parse_chart_xml(&x).unwrap().expect("series found");
        let (title, cats, series) = &chart;
        assert_eq!(title, "Employees by region");
        assert_eq!(*cats, ["North America", "EMEA", "Asia Pacific"]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].0, "Share");
        assert_eq!(series[0].1, ["34.5", "28.0", "17.5"]);

        let els = chart_element(&chart, 0, &mut Budget::default()).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0], vec!["", "Share"]);
        assert_eq!(cells[2], vec!["EMEA", "28.0"], "EMEA keeps its own value");
        assert_eq!(t.evidence, "pptx-chart");
    }

    #[test]
    fn multiple_series_become_multiple_columns() {
        let x = chart_xml(&format!(
            "{}{}",
            ser("Americas", &["2023", "2024"], &["31.2", "34.5"]),
            ser("EMEA", &["2023", "2024"], &["24.1", "28.0"])
        ));
        let chart = parse_chart_xml(&x).unwrap().unwrap();
        let (_, cats, series) = &chart;
        assert_eq!(cats, &["2023", "2024"], "categories are read once");
        assert_eq!(series.len(), 2);
        let els = chart_element(&chart, 0, &mut Budget::default()).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        let cells = t.cells.as_ref().unwrap();
        assert_eq!(cells[0], vec!["", "Americas", "EMEA"]);
        assert_eq!(cells[2], vec!["2024", "34.5", "28.0"]);
    }

    #[test]
    fn a_part_with_no_series_is_not_a_chart() {
        assert!(parse_chart_xml("<c:chartSpace/>").unwrap().is_none());
    }

    #[test]
    fn a_chart_keeps_each_point_at_its_index() {
        // B has no value: PowerPoint writes no point for it, and C's value
        // must not move up to B.
        let x = chart_xml(
            "<c:ser><c:tx><c:v>Revenue</c:v></c:tx>\
             <c:cat><c:strLit><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>A</c:v></c:pt>\
             <c:pt idx=\"1\"><c:v>B</c:v></c:pt><c:pt idx=\"2\"><c:v>C</c:v></c:pt></c:strLit></c:cat>\
             <c:val><c:numLit><c:ptCount val=\"3\"/><c:pt idx=\"0\"><c:v>10</c:v></c:pt>\
             <c:pt idx=\"2\"><c:v>30</c:v></c:pt></c:numLit></c:val></c:ser>",
        );
        let (_, cats, series) = parse_chart_xml(&x).unwrap().unwrap();
        assert_eq!(cats, ["A", "B", "C"]);
        assert_eq!(series[0].1, ["10", "", "30"]);
        // A trailing point left out still has its row.
        let x = chart_xml(
            "<c:ser><c:cat><c:strLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>A</c:v></c:pt>\
             <c:pt idx=\"1\"><c:v>B</c:v></c:pt></c:strLit></c:cat>\
             <c:val><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>5</c:v></c:pt></c:numLit></c:val></c:ser>",
        );
        assert_eq!(parse_chart_xml(&x).unwrap().unwrap().2[0].1, ["5", ""]);
    }

    #[test]
    fn a_multi_level_axis_reads_the_level_next_to_the_plot() {
        let x = chart_xml(
            "<c:ser><c:cat><c:multiLvlStrRef><c:multiLvlStrCache><c:ptCount val=\"2\"/>\
             <c:lvl><c:pt idx=\"0\"><c:v>Q1</c:v></c:pt><c:pt idx=\"1\"><c:v>Q2</c:v></c:pt></c:lvl>\
             <c:lvl><c:pt idx=\"0\"><c:v>2026</c:v></c:pt></c:lvl>\
             </c:multiLvlStrCache></c:multiLvlStrRef></c:cat>\
             <c:val><c:numLit><c:pt idx=\"0\"><c:v>1</c:v></c:pt><c:pt idx=\"1\"><c:v>2</c:v></c:pt></c:numLit></c:val></c:ser>",
        );
        assert_eq!(parse_chart_xml(&x).unwrap().unwrap().1, ["Q1", "Q2"]);
    }

    #[test]
    fn a_charts_declared_points_are_spent_before_they_are_made() {
        // One series declaring 100,000 points it does not write: refused
        // by a budget of 8,192 cells before its values are laid out.
        let x = chart_xml(
            "<c:ser><c:val><c:numCache><c:ptCount val=\"100000\"/></c:numCache></c:val></c:ser>",
        );
        let mut small = Budget::new(8_192 * fluree_doc_model::budget::CELL_BYTES, 0);
        assert!(parse_chart(&x, &mut small).is_err());
        assert!(parse_chart(&x, &mut Budget::default()).unwrap().is_some());
    }

    #[test]
    fn a_tables_cell_costs_what_a_sheets_does() {
        use fluree_doc_model::budget::{Output, ELEMENT_BYTES, FLAT_CELL_BYTES, TEXT_BYTES};
        let mut e = Element::new("doco:Table", String::new(), "pptx", "pptx");
        e.cells = Some(vec![vec!["ab".into(), String::new()]]);
        let spend =
            |output, memory| spend_table(&e, &mut Budget::new(memory, 0).for_output(output));
        // The table, and its cell that holds text written once in its row,
        // or as a node of its own in DoCO, a node at a time, at the same
        // cost; its grid was spent as it was made.
        let need = ELEMENT_BYTES + FLAT_CELL_BYTES + 2 * TEXT_BYTES;
        for output in [Output::Flat, Output::Doco] {
            assert!(spend(output, need).is_ok());
            assert!(spend(output, need - 1).is_err());
        }
    }

    /// A stored package of `parts`.
    fn package(parts: &[(String, String)]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in parts {
            z.start_file(name.as_str(), opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn a_slide_listed_is_found_among_the_relationships_in_one_lookup() {
        // 20,000 slides listed by an id none of 200,000 relationships has,
        // then one that is: a scan of them all for each listing took 24 s
        // in a release build.
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
        let bytes = |n: usize| {
            let listed: String = (0..n)
                .map(|i| format!("<p:sldId id=\"{}\" r:id=\"rIdXXXXXXX\"/>", 256 + i))
                .collect();
            let others: String = (0..10 * n)
                .map(|i| format!("<Relationship Id=\"rId{i:07}\" Target=\"a\"/>"))
                .collect();
            let parts = vec![
                (
                    "ppt/presentation.xml".to_string(),
                    format!(
                        "<p:presentation {NS}><p:sldIdLst>{listed}\
                     <p:sldId id=\"1\" r:id=\"rId1\"/></p:sldIdLst></p:presentation>"
                    ),
                ),
                (
                    "ppt/_rels/presentation.xml.rels".to_string(),
                    format!(
                        "<Relationships>{others}<Relationship Id=\"rId1\" Type=\"{rel}\" \
                     Target=\"slides/slide1.xml\"/></Relationships>"
                    ),
                ),
                (
                    "ppt/slides/slide1.xml".to_string(),
                    slide(&shape(None, &para("listed"))),
                ),
            ];
            package(&parts)
        };
        assert_linear("slides listed among relationships", 1_000, bytes, |b| {
            parse(b).unwrap();
        });
        let els = parse(&bytes(2_000)).unwrap();
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["listed"]);
    }

    #[test]
    fn a_slides_part_is_found_among_the_packages_in_one_lookup() {
        // 200,000 slides listed by a relationship to a part the package
        // does not hold, among 30,000 parts: a scan of the parts for each
        // listing took 4.7 s in a release build. A deck that lists no part it
        // holds is read in its parts' order.
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
        let bytes = |n: usize| {
            let listed = "<p:sldId id=\"256\" r:id=\"rId1\"/>".repeat(20 * n);
            let mut parts =
                vec![
            (
                "ppt/presentation.xml".to_string(),
                format!("<p:presentation {NS}><p:sldIdLst>{listed}</p:sldIdLst></p:presentation>"),
            ),
            (
                "ppt/_rels/presentation.xml.rels".to_string(),
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}\" \
                     Target=\"slides/gone.xml\"/></Relationships>"
                ),
            ),
            (
                "ppt/slides/slide1.xml".to_string(),
                slide(&shape(None, &para("unlisted"))),
            ),
        ];
            parts.extend((0..3 * n).map(|i| (format!("ppt/other/p{i}.xml"), String::new())));
            package(&parts)
        };
        assert_linear("slides listed among parts", 500, bytes, |b| {
            parse(b).unwrap();
        });
        let els = parse(&bytes(1_000)).unwrap();
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["unlisted"]);
    }

    #[test]
    fn a_frames_chart_is_found_among_the_slides_in_one_lookup() {
        // 20,000 frames naming an id none of the slide's 200,000 chart
        // relationships has: a scan of them all for each frame took 24 s in
        // a release build.
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
        let frame = "<p:graphicFrame><a:graphic><a:graphicData>\
                     <c:chart xmlns:c=\"c\" r:id=\"rIdXXXXXXX\"/></a:graphicData></a:graphic></p:graphicFrame>";
        let bytes = |n: usize| {
            let charts: String = (0..10 * n)
            .map(|i| {
                format!(
                    "<Relationship Id=\"rId{i:07}\" Type=\"{rel}\" Target=\"../charts/chart1.xml\"/>"
                )
            })
            .collect();
            let parts = vec![
                (
                    "ppt/charts/chart1.xml".to_string(),
                    chart_xml(&ser("Share", &["EMEA"], &["28.0"])),
                ),
                ("ppt/slides/slide1.xml".to_string(), slide(&frame.repeat(n))),
                (
                    "ppt/slides/_rels/slide1.xml.rels".to_string(),
                    format!("<Relationships>{charts}</Relationships>"),
                ),
            ];
            package(&parts)
        };
        assert_linear("frames among chart relationships", 1_000, bytes, |b| {
            parse(b).unwrap();
        });
        let els = parse(&bytes(2_000)).unwrap();
        assert!(els.iter().all(|e| e.kind != "doco:Table"));
    }

    #[test]
    fn a_slides_relationships_are_read_once_however_often_it_is_listed() {
        // A slide of 200,000 chart relationships listed 200 times: its
        // relationships read and resolved again for each listing, 300,000
        // listed 100 times took 17 s in a release build.
        let chart = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
        let slide_rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
        let frame = "<p:graphicFrame><a:graphic><a:graphicData>\
                     <c:chart xmlns:c=\"c\" r:id=\"rId0000007\"/></a:graphicData></a:graphic></p:graphicFrame>";
        let bytes = |n: usize| {
            let charts: String = (0..1_000 * n)
            .map(|i| {
                format!(
                    "<Relationship Id=\"rId{i:07}\" Type=\"{chart}\" Target=\"../charts/chart1.xml\"/>"
                )
            })
            .collect();
            let listed = "<p:sldId id=\"256\" r:id=\"rIdS\"/>".repeat(n);
            let parts =
                vec![
            (
                "ppt/presentation.xml".to_string(),
                format!("<p:presentation {NS}><p:sldIdLst>{listed}</p:sldIdLst></p:presentation>"),
            ),
            (
                "ppt/_rels/presentation.xml.rels".to_string(),
                format!(
                    "<Relationships><Relationship Id=\"rIdS\" Type=\"{slide_rel}\" \
                     Target=\"slides/slide1.xml\"/></Relationships>"
                ),
            ),
            (
                "ppt/charts/chart1.xml".to_string(),
                chart_xml(&ser("Share", &["EMEA"], &["28.0"])),
            ),
            ("ppt/slides/slide1.xml".to_string(), slide(frame)),
            (
                "ppt/slides/_rels/slide1.xml.rels".to_string(),
                format!("<Relationships>{charts}</Relationships>"),
            ),
        ];
            package(&parts)
        };
        assert_linear("a slide listed many times", 10, bytes, |b| {
            parse(b).unwrap();
        });
        let els = parse(&bytes(20)).unwrap();
        let tables = els.iter().filter(|e| e.kind == "doco:Table").count();
        assert_eq!(tables, 20, "the chart, at each listing");
    }

    #[test]
    fn a_chart_named_by_many_frames_is_read_once() {
        // A chart part of 1 MiB, named by each of 1,300 slides and twice by
        // the first: read once, it spends 2 MiB of the budget; read for
        // each slide, 2.6 GB, past it.
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
        let frame = "<p:graphicFrame><a:graphic><a:graphicData>\
                     <c:chart xmlns:c=\"c\" r:id=\"rId1\"/></a:graphicData></a:graphic></p:graphicFrame>";
        let padded = format!(
            "{}{}",
            ser("Share", &["EMEA"], &["28.0"]),
            " ".repeat(1 << 20)
        );
        let mut parts = vec![("ppt/charts/chart1.xml".to_string(), chart_xml(&padded))];
        let slides = 1_300;
        for k in 1..=slides {
            let frames = if k == 1 { 2 } else { 1 };
            parts.push((
                format!("ppt/slides/slide{k}.xml"),
                slide(&frame.repeat(frames)),
            ));
            parts.push((
                format!("ppt/slides/_rels/slide{k}.xml.rels"),
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}\" \
                     Target=\"../charts/chart1.xml\"/></Relationships>"
                ),
            ));
        }
        let els = parse(&package(&parts)).unwrap();
        let tables = els.iter().filter(|e| e.kind == "doco:Table").count();
        assert_eq!(tables, slides + 1, "one table for each frame");
    }

    #[test]
    fn slides_are_read_in_the_decks_order_not_their_parts_names() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
        for (name, body) in [
            (
                "ppt/presentation.xml",
                format!(
                    "<p:presentation {NS}><p:sldIdLst><p:sldId id=\"257\" r:id=\"rId2\"/>\
                     <p:sldId id=\"256\" r:id=\"rId1\"/></p:sldIdLst></p:presentation>"
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{rel}\" Target=\"slides/slide1.xml\"/>\
                     <Relationship Id=\"rId2\" Type=\"{rel}\" Target=\"/ppt/slides/slide2.xml\"/></Relationships>"
                ),
            ),
            ("ppt/slides/slide1.xml", slide(&shape(None, &para("moved to second")))),
            ("ppt/slides/slide2.xml", slide(&shape(None, &para("moved to first")))),
        ] {
            z.start_file(name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let els = parse(buf.get_ref()).unwrap();
        let read: Vec<(usize, &str)> = els.iter().map(|e| (e.page, e.text.as_str())).collect();
        assert_eq!(read, [(0, "moved to first"), (1, "moved to second")]);
    }

    #[test]
    fn nothing_claims_geometry() {
        let x = slide(&shape(Some("title"), &para("T")));
        let els = parse_slide_xml(&x, 0).unwrap();
        assert!(els.iter().all(|e| e.bbox.is_none()));
        assert!(els.iter().all(|e| e.provenance == "pptx"));
    }

    #[test]
    fn a_non_archive_is_an_error_not_a_panic() {
        assert!(parse(b"not a zip").is_err());
    }

    /// A slide with a table of one column: `first`, then `rows - 1` cells
    /// continuing it (`vMerge`).
    fn merged_down(first: &str, rows: usize) -> String {
        let row = |attr: &str, text: &str| {
            format!(
                "<a:tr><a:tc{attr}><a:txBody>{}</a:txBody></a:tc></a:tr>",
                para(text)
            )
        };
        slide(&format!(
            "<p:graphicFrame><a:graphic><a:graphicData><a:tbl>{}{}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>",
            row("", first),
            row(" vMerge=\"1\"", "").repeat(rows - 1)
        ))
    }

    #[test]
    fn a_long_value_merged_down_is_refused_rather_than_copied_into_each_row() {
        let long = "y".repeat(200_000);
        assert!(parse_slide_xml(&merged_down(&long, 3), 0).is_ok());
        let err = parse_slide_xml(&merged_down(&long, 3_000), 0).unwrap_err();
        assert!(matches!(err, PptxError::TooLarge(_)), "{err}");
    }

    #[test]
    fn a_span_past_the_grid_is_cut_at_its_edge() {
        // A span of 2^63 columns over a table two wide marks one.
        let x = slide(&format!(
            "<p:graphicFrame><a:graphic><a:graphicData><a:tbl>\
               <a:tr><a:tc gridSpan=\"9223372036854775807\"><a:txBody>{}</a:txBody></a:tc>\
                     <a:tc hMerge=\"1\"><a:txBody>{}</a:txBody></a:tc></a:tr>\
             </a:tbl></a:graphicData></a:graphic></p:graphicFrame>",
            para("Banner"),
            para("")
        ));
        let els = parse_slide_xml(&x, 0).unwrap();
        let t = els.iter().find(|e| e.kind == "doco:Table").unwrap();
        assert_eq!(*t.merged_left.as_ref().unwrap(), [false, true]);
    }

    #[test]
    fn a_tables_grid_is_spent_before_it_is_made() {
        // One row of 4,000 cells over 4,000 rows of one: sixteen million
        // cells of padding.
        let wide = format!("<a:tr>{}</a:tr>", "<a:tc></a:tc>".repeat(4_000));
        let tall = "<a:tr><a:tc></a:tc></a:tr>".repeat(4_000);
        let x = slide(&format!(
            "<p:graphicFrame><a:graphic><a:graphicData><a:tbl>{wide}{tall}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
        ));
        let mut small = Budget::new(1 << 30, 0);
        let err = parse_slide(&x, 0, &|_| None, &mut small).unwrap_err();
        assert!(matches!(err, PptxError::TooLarge(_)), "{err}");
    }
}
