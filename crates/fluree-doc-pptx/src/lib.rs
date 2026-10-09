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
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Read;

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
            Self::NoSlides => write!(f, "archive has no ppt/slides/slideN.xml"),
        }
    }
}

impl std::error::Error for PptxError {}

/// Parse a `.pptx` file's bytes into elements, in slide order.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, PptxError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| PptxError::Zip(e.to_string()))?;

    let slides = slide_parts(&mut zip);
    if slides.is_empty() {
        return Err(PptxError::NoSlides);
    }

    let mut out = Vec::new();
    for (idx, name) in slides.iter().enumerate() {
        let mut xml = String::new();
        if zip
            .by_name(name)
            .map_err(|e| PptxError::Zip(e.to_string()))?
            .read_to_string(&mut xml)
            .is_err()
        {
            continue;
        }
        // Charts live in their own parts; the slide only names a
        // relationship id, so resolve those before walking it.
        let mut charts: Vec<(String, String)> = Vec::new();
        let mut rels = String::new();
        if let Ok(mut f) = zip.by_name(&rels_part(name)) {
            let _ = f.read_to_string(&mut rels);
        }
        for (id, part) in chart_parts_for(&rels, name) {
            let mut cx = String::new();
            if let Ok(mut f) = zip.by_name(&part) {
                if f.read_to_string(&mut cx).is_ok() {
                    charts.push((id, cx));
                }
            }
        }
        out.extend(parse_slide_with_charts(&xml, idx, &charts)?);
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    Ok(out)
}

/// The deck's slide parts, in its order.
///
/// `ppt/presentation.xml` lists the slides as relationship ids
/// (`<p:sldId r:id="rId2"/>`), and its relationships name each one's part.
/// A package without that list, or whose list names no part it holds, is
/// read in the numeric order of its slide parts' names, where `slide10`
/// comes after `slide2`.
fn slide_parts(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Vec<String> {
    let mut read = |name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let listed = read("ppt/presentation.xml")
        .zip(read("ppt/_rels/presentation.xml.rels"))
        .map(|(presentation, rels)| {
            let targets = relationships(&rels, "ppt/presentation.xml");
            slide_ids(&presentation)
                .into_iter()
                .filter_map(|id| targets.iter().find(|(i, _, _)| *i == id))
                .map(|(_, _, part)| part.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let listed: Vec<String> = listed.into_iter().filter(|p| names.contains(p)).collect();
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

/// A part's relationships: `dir/name.xml` → `dir/_rels/name.xml.rels`.
fn rels_part(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// A part's relationships as (id, type, archive path), each target resolved
/// against the directory of `source`, the part they belong to.
fn relationships(rels_xml: &str, source: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(rels_xml);
    let mut buf = Vec::new();
    let dir = source.rsplit_once('/').map_or("", |(d, _)| d);
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == "Relationship" => {
                // An external target is a URL, not a part.
                if attr(&e, "TargetMode").as_deref() == Some("External") {
                    continue;
                }
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    let ty = attr(&e, "Type").unwrap_or_default();
                    out.push((id, ty, resolve_part(dir, &target)));
                }
            }
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A relationship target as an archive path: absolute from the package
/// root when it starts with `/`, otherwise relative to `dir`, with `.` and
/// `..` segments applied.
fn resolve_part(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Chart parts a slide references, in relationship order, resolved against
/// the slide's own directory.
fn chart_parts_for(rels_xml: &str, slide: &str) -> Vec<(String, String)> {
    relationships(rels_xml, slide)
        .into_iter()
        .filter(|(_, ty, _)| ty.ends_with("/chart"))
        .map(|(id, _, part)| (id, part))
        .collect()
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

/// What the file declares about itself in its core properties
/// (`docProps/core.xml`): title, author, and when it was made and last
/// saved. Empty where the part is missing or the file is not a package.
pub fn info(bytes: &[u8]) -> DocumentInfo {
    let mut xml = String::new();
    let read = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .ok()
        .and_then(|mut zip| {
            zip.by_name("docProps/core.xml")
                .ok()?
                .read_to_string(&mut xml)
                .ok()
        });
    if read.is_some() {
        DocumentInfo::from_core_properties(&xml)
    } else {
        DocumentInfo::default()
    }
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
                            para_bullet || para_level > 0,
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
                    flush(&mut out, &mut text, page, shape, false);
                    shape = Shape::Body;
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    flush(&mut out, &mut text, page, Shape::Body, false);
    Ok(out)
}

fn local(q: &[u8]) -> &str {
    let s = std::str::from_utf8(q).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s)
}

fn attr(e: &quick_xml::events::BytesStart<'_>, want: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (local(a.key.as_ref()) == want)
            .then(|| String::from_utf8_lossy(a.value.as_ref()).to_string())
    })
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
        id: String::new(),
        kind: kind.into(),
        page,
        bbox: None,
        text,
        level,
        cells: None,
        header_rows: None,
        sub_headers: None,
        merged_down: None,
        merged_left: None,
        figure: None,
        links: None,
        turn: None,
        message: None,
        resumes: None,
        signature: false,
        datums: None,
        provenance: "pptx",
        evidence: "pptx",
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
}

fn flush(out: &mut Vec<Element>, buf: &mut String, page: usize, shape: Shape, listy: bool) {
    let t = std::mem::take(buf)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if t.is_empty() {
        return;
    }
    if shape == Shape::DeckTitle {
        out.push(element("doco:Title", t, None, page));
    } else if shape == Shape::SlideTitle {
        out.push(element("doco:SectionTitle", t, Some(1), page));
    } else if listy {
        out.push(element("doc:ListItem", t, None, page));
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
        assert_eq!(els[1].kind, "doco:Paragraph");
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
    fn chart_relationships_resolve_to_archive_paths() {
        let rels = "<Relationships><Relationship Id=\"rId1\" \
            Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" \
            Target=\"../slideLayouts/slideLayout6.xml\"/>\
            <Relationship Id=\"rId2\" \
            Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" \
            Target=\"../charts/chart1.xml\"/></Relationships>";
        let parts = chart_parts_for(rels, "ppt/slides/slide1.xml");
        assert_eq!(
            parts,
            [("rId2".to_string(), "ppt/charts/chart1.xml".to_string())]
        );
        assert_eq!(
            resolve_part("ppt", "/ppt/slides/slide3.xml"),
            "ppt/slides/slide3.xml"
        );
        assert_eq!(
            resolve_part("ppt", "./slides/slide3.xml"),
            "ppt/slides/slide3.xml"
        );
        assert_eq!(
            rels_part("ppt/slides/slide3.xml"),
            "ppt/slides/_rels/slide3.xml.rels"
        );
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
