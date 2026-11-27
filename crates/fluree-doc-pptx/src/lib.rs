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

use fluree_doc_model::{DocumentInfo, Element};
use fluree_doc_ooxml::{attr, local, Package};
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug)]
pub enum PptxError {
    Zip(String),
    Xml(String),
    NoSlides,
}

impl std::fmt::Display for PptxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zip(e) => write!(f, "not a readable .pptx: {e}"),
            Self::Xml(e) => write!(f, "malformed slide XML: {e}"),
            Self::NoSlides => write!(f, "package has no slides"),
        }
    }
}

impl std::error::Error for PptxError {}

/// Parse a `.pptx` file's bytes into elements, in slide order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, PptxError> {
    parse_with_info(bytes).map(|(elements, _)| elements)
}

/// [`parse`], with what the file declares about itself in its core
/// properties, read from the one opening of the package.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), PptxError> {
    let mut pkg = Package::open(bytes).map_err(PptxError::Zip)?;
    let slides = slide_parts(&mut pkg);
    if slides.is_empty() {
        return Err(PptxError::NoSlides);
    }

    let mut out = Vec::new();
    let mut notes = Vec::new();
    for (idx, name) in slides.iter().enumerate() {
        let Some(xml) = pkg.read(name) else {
            continue;
        };
        // The slide's speaker notes, by the slide's own relationship to its
        // notes part: `notesSlide3` can be slide 4's.
        let part = pkg
            .relationships(name)
            .into_iter()
            .find(|r| r.is("notesSlide") && !r.external)
            .map(|r| r.target);
        if let Some(nx) = part.and_then(|p| pkg.read(&p)) {
            notes.extend(parse_notes_xml(&nx, idx)?);
        }
        // Charts live in their own parts; the slide only names a
        // relationship id, so resolve those before walking it.
        let charts: Vec<(String, String)> = pkg
            .relationships(name)
            .into_iter()
            .filter(|r| r.is("chart") && !r.external)
            .filter_map(|r| Some((r.id, pkg.read(&r.target)?)))
            .collect();
        out.extend(parse_slide_with_charts(&xml, idx, &charts)?);
    }
    // After the last slide, in slide order: see `Element::notes_for`.
    out.extend(notes);
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
fn slide_parts(pkg: &mut Package<'_>) -> Vec<String> {
    let presentation = pkg.main_part("ppt/presentation.xml");
    let rels = pkg.relationships(&presentation);
    let names = pkg.part_names();
    let listed: Vec<String> = pkg
        .read(&presentation)
        .map(|xml| slide_ids(&xml))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| rels.iter().find(|r| r.id == id && !r.external))
        .map(|r| r.target.clone())
        .filter(|p| names.contains(p))
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    let mut numbered: Vec<(usize, String)> = names
        .into_iter()
        .filter_map(|n| slide_number(&n).map(|k| (k, n)))
        .collect();
    numbered.sort();
    numbered.into_iter().map(|(_, n)| n).collect()
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
                let rel = e.attributes().flatten().find(|a| {
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

/// Read a chart part. Values come from the cached `c:strCache`/`c:numCache`
/// blocks, which hold what the chart actually plots — the `c:f` formula
/// beside them points into a workbook that may not travel with the deck.
///
/// A cache states how many points it has (`c:ptCount`) and each point's
/// place (`c:pt idx`), and leaves out the ones with no value: a bar not
/// drawn is a point not written. So each value goes to its index, and a
/// series' gaps stay gaps instead of moving its later values onto earlier
/// categories. Of a multi-level category axis, the first level is read: the
/// labels next to the plot, one per point.
pub fn parse_chart_xml(xml: &str) -> Option<ChartData> {
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
                                        pending.push((at, val.trim().to_string()));
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
                    let points = by_index(&mut pending, &mut count);
                    if chart.categories.is_empty() {
                        chart.categories = points;
                    }
                    slot = Slot::None;
                    level = 0;
                }
                "val" | "yVal" => {
                    let points = by_index(&mut pending, &mut count);
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
    (!chart.series.is_empty()).then_some((chart.title, chart.categories, chart.series))
}

/// A cache's values laid out by index: as many as it declares or holds,
/// blank where it wrote no point. Leaves the cache state empty for the next.
fn by_index(points: &mut Vec<(usize, String)>, count: &mut usize) -> Vec<String> {
    let n = points
        .iter()
        .map(|(i, _)| i + 1)
        .max()
        .unwrap_or(0)
        .max(*count);
    let mut out = vec![String::new(); n];
    for (i, v) in points.drain(..) {
        out[i] = v;
    }
    *count = 0;
    out
}

/// A chart as a table: categories down the first column, one column per
/// series. The numbers become cells, so a consumer can ask what EMEA's share
/// was without looking at a picture.
fn chart_element(
    title: String,
    categories: Vec<String>,
    series: Vec<Series>,
    page: usize,
) -> Vec<Element> {
    let mut out = Vec::new();
    if !title.is_empty() {
        out.push(element("doco:SectionTitle", title, Some(2), page));
    }
    let mut header = vec![String::new()];
    for (name, _) in &series {
        header.push(name.clone());
    }
    let n = categories
        .len()
        .max(series.iter().map(|(_, v)| v.len()).max().unwrap_or(0));
    let mut rows = vec![header];
    for i in 0..n {
        let mut row = vec![categories.get(i).cloned().unwrap_or_default()];
        for (_, vals) in &series {
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
    out.push(e);
    out
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
                        if let Some((_, cx)) = charts.iter().find(|(rid, _)| *rid == id) {
                            if let Some((title, cats, series)) = parse_chart_xml(cx) {
                                out.extend(chart_element(title, cats, series, page));
                            }
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
                            page,
                            shape,
                            (para_bullet || para_level > 0).then_some(para_level),
                        );
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
                        emit_table(&mut out, std::mem::take(&mut rows), page);
                    }
                }
                "sp" => {
                    // A shape's title-ness does not leak into the next shape.
                    flush(&mut out, &mut text, page, shape, None);
                    shape = Shape::Body;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    flush(&mut out, &mut text, page, Shape::Body, None);
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

/// Parse a slide's notes part into the elements of its speaker notes,
/// tagged with the slide's index as `page` and `notes_for`.
///
/// The notes are the page's `body` placeholder: paragraphs, and list items
/// where a paragraph carries a bullet or an outline level, as on a slide.
/// The rest of a notes page is the slide's picture, its number, and the
/// header, footer and date the notes master repeats on every page.
pub fn parse_notes_xml(xml: &str, page: usize) -> Result<Vec<Element>, PptxError> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<Element> = Vec::new();
    let mut text = String::new();
    let mut in_text = false;
    let mut in_body = false;
    let mut para_level = 0usize;
    let mut para_bullet = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(e) => return Err(PptxError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "sp" => in_body = false,
                "ph" => in_body = attr(&e, "type").as_deref() == Some("body"),
                "t" => in_text = in_body,
                "br" if in_body => text.push(' '),
                "pPr" => {
                    para_level = attr(&e, "lvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                "buChar" | "buAutoNum" => para_bullet = true,
                "buNone" => para_bullet = false,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_text {
                    text.push_str(&t.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "t" => in_text = false,
                "p" => {
                    if in_body {
                        let item = (para_bullet || para_level > 0).then_some(para_level);
                        flush(&mut out, &mut text, page, Shape::Body, item);
                    }
                    text.clear();
                    para_bullet = false;
                    para_level = 0;
                }
                "sp" => {
                    text.clear();
                    in_body = false;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    for e in &mut out {
        e.notes_for = Some(page);
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

/// `item` is a list item's depth, its paragraph's outline level `lvl`, for
/// a paragraph read as one.
fn flush(out: &mut Vec<Element>, buf: &mut String, page: usize, shape: Shape, item: Option<usize>) {
    let t = std::mem::take(buf)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() {
        return;
    }
    if shape == Shape::DeckTitle {
        out.push(element("doco:Title", t, None, page));
    } else if shape == Shape::DeckSubtitle && !out.iter().any(|e| e.kind == "doco:Subtitle") {
        out.push(element("doco:Subtitle", t, None, page));
    } else if shape == Shape::SlideTitle {
        out.push(element("doco:SectionTitle", t, Some(1), page));
    } else if let Some(depth) = item {
        out.push(Element {
            depth,
            ..element("doc:ListItem", t, None, page)
        });
    } else {
        out.push(element("doco:Paragraph", t, None, page));
    }
}

fn emit_table(out: &mut Vec<Element>, rows: Vec<Vec<Cell>>, page: usize) {
    let rows: Vec<Vec<Cell>> = rows.into_iter().filter(|r| !r.is_empty()).collect();
    if rows.is_empty() {
        return;
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if width == 0 {
        return;
    }
    let n = rows.len();
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
            for k in 1..cell.span.max(1) {
                if c + k < width {
                    m_left[r * width + c + k] = true;
                }
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
    out.push(e);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (title, cats, series) = parse_chart_xml(&x).expect("series found");
        assert_eq!(title, "Employees by region");
        assert_eq!(cats, ["North America", "EMEA", "Asia Pacific"]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].0, "Share");
        assert_eq!(series[0].1, ["34.5", "28.0", "17.5"]);

        let els = chart_element(title, cats, series, 0);
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
        let (_, cats, series) = parse_chart_xml(&x).unwrap();
        assert_eq!(cats, ["2023", "2024"], "categories are read once");
        assert_eq!(series.len(), 2);
        let els = chart_element(String::new(), cats, series, 0);
        let cells = els[0].cells.as_ref().unwrap();
        assert_eq!(cells[0], vec!["", "Americas", "EMEA"]);
        assert_eq!(cells[2], vec!["2024", "34.5", "28.0"]);
    }

    #[test]
    fn a_part_with_no_series_is_not_a_chart() {
        assert!(parse_chart_xml("<c:chartSpace/>").is_none());
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
        let (_, cats, series) = parse_chart_xml(&x).unwrap();
        assert_eq!(cats, ["A", "B", "C"]);
        assert_eq!(series[0].1, ["10", "", "30"]);
        // A trailing point left out still has its row.
        let x = chart_xml(
            "<c:ser><c:cat><c:strLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>A</c:v></c:pt>\
             <c:pt idx=\"1\"><c:v>B</c:v></c:pt></c:strLit></c:cat>\
             <c:val><c:numLit><c:ptCount val=\"2\"/><c:pt idx=\"0\"><c:v>5</c:v></c:pt></c:numLit></c:val></c:ser>",
        );
        assert_eq!(parse_chart_xml(&x).unwrap().2[0].1, ["5", ""]);
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
        assert_eq!(parse_chart_xml(&x).unwrap().1, ["Q1", "Q2"]);
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

    fn notes_page(body: &str) -> String {
        format!("<p:notes {NS}><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:notes>")
    }

    #[test]
    fn a_notes_page_is_read_from_its_body_placeholder() {
        let x = notes_page(&format!(
            "{}{}{}{}{}",
            shape(Some("sldImg"), ""),
            shape(
                Some("body"),
                &format!(
                    "{}<a:p><a:pPr><a:buChar char=\"•\"/></a:pPr><a:r><a:t>A point</a:t></a:r></a:p>",
                    para("What the presenter says.")
                )
            ),
            shape(Some("sldNum"), &para("4")),
            shape(Some("ftr"), &para("Footer")),
            shape(None, &para("4")),
        ));
        let els = parse_notes_xml(&x, 3).unwrap();
        let read: Vec<(&str, &str)> = els
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                ("doco:Paragraph", "What the presenter says."),
                ("doc:ListItem", "A point")
            ]
        );
        assert!(els.iter().all(|e| e.notes_for == Some(3) && e.page == 3));
    }

    #[test]
    fn a_slides_notes_follow_the_last_slide_by_its_own_relationship() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let base = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let notes_rel = |target: &str| {
            format!(
                "<Relationships><Relationship Id=\"rId9\" Type=\"{base}/notesSlide\" \
                 Target=\"../notesSlides/{target}\"/></Relationships>"
            )
        };
        for (name, body) in [
            (
                "ppt/presentation.xml",
                format!(
                    "<p:presentation {NS}><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/>\
                     <p:sldId id=\"257\" r:id=\"rId2\"/></p:sldIdLst></p:presentation>"
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                format!(
                    "<Relationships><Relationship Id=\"rId1\" Type=\"{base}/slide\" Target=\"slides/slide1.xml\"/>\
                     <Relationship Id=\"rId2\" Type=\"{base}/slide\" Target=\"slides/slide2.xml\"/></Relationships>"
                ),
            ),
            ("ppt/slides/slide1.xml", slide(&shape(None, &para("Slide one")))),
            ("ppt/slides/slide2.xml", slide(&shape(None, &para("Slide two")))),
            // Named the other way round: the relationship says whose they are.
            ("ppt/slides/_rels/slide1.xml.rels", notes_rel("notesSlide2.xml")),
            ("ppt/slides/_rels/slide2.xml.rels", notes_rel("notesSlide1.xml")),
            ("ppt/notesSlides/notesSlide2.xml", notes_page(&shape(Some("body"), &para("About one")))),
            ("ppt/notesSlides/notesSlide1.xml", notes_page(&shape(Some("body"), &para("About two")))),
        ] {
            z.start_file(name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let els = parse(buf.get_ref()).unwrap();
        let read: Vec<(&str, Option<usize>)> =
            els.iter().map(|e| (e.text.as_str(), e.notes_for)).collect();
        assert_eq!(
            read,
            [
                ("Slide one", None),
                ("Slide two", None),
                ("About one", Some(0)),
                ("About two", Some(1))
            ]
        );
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
}
