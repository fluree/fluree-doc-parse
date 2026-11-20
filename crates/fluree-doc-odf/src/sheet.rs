//! An OpenDocument spreadsheet's sheets, for the workbook layout.
//!
//! A repeated cell that holds something is a cell, and its text a copy,
//! each time it repeats: the cells are counted against [`MAX_CELLS`], and
//! each cell and its text spent from the document's [`Budget`], before it
//! is made.

use crate::tree::Tree;
use crate::{OdfError, MAX_CELLS};
use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{Datum, Element};
use fluree_doc_xlsx::{GridCell, GridSheet};
use std::collections::HashSet;

/// Excel's grid: no row or column past these is read. A sheet's trailing
/// empty rows and columns are written as one row repeated to the end of it.
const MAX_ROWS: u64 = 1 << 20;
const MAX_COLUMNS: u64 = 1 << 14;

pub(crate) fn elements(content: &Tree) -> Result<Vec<Element>, OdfError> {
    let bold = bold_styles(content);
    let mut sheets = Vec::new();
    let mut total = 0usize;
    // A repeated cell is its text again each time: spent before each copy,
    // as the cell is.
    let mut budget = Budget::default();
    let Some(book) = content.find(0, "spreadsheet") else {
        return Ok(Vec::new());
    };
    for t in content
        .elements(book)
        .filter(|&t| content.node(t).name == "table")
    {
        let mut g = GridSheet {
            name: content.attr(t, "name").unwrap_or("").to_string(),
            ..GridSheet::default()
        };
        let mut row = 0u64;
        let mut rows = Vec::new();
        rows_of(content, t, false, &mut rows);
        let mut header = 0u32;
        for (r, is_header) in rows {
            if row >= MAX_ROWS {
                break;
            }
            let repeat = count(content.attr(r, "number-rows-repeated"));
            let cells = cells_of(content, r, &bold, &mut budget)?;
            if is_header && row == u64::from(header) {
                header += repeat.min(16) as u32;
            }
            if cells.is_empty() {
                row = row.saturating_add(repeat);
                continue;
            }
            for _ in 0..repeat {
                if row >= MAX_ROWS {
                    break;
                }
                for c in &cells {
                    total += 1;
                    if total > MAX_CELLS {
                        return Err(OdfError::TooLarge(format!(
                            "its sheets hold more than {MAX_CELLS} cells"
                        )));
                    }
                    spend(&mut budget, &c.cell.text)?;
                    g.cells.push(GridCell {
                        row: row as u32,
                        ..c.cell.clone()
                    });
                    if let Some((cs, rs)) = c.span {
                        g.merges.push((
                            row as u32,
                            c.cell.col,
                            row as u32 + rs - 1,
                            c.cell.col + cs - 1,
                        ));
                    }
                }
                row += 1;
            }
        }
        g.frozen_rows = header;
        sheets.push(g);
    }
    Ok(fluree_doc_xlsx::grid_elements(sheets, "odf"))
}

/// A row's cells that hold something, at their columns, repeats counted.
struct RowCell {
    cell: GridCell,
    span: Option<(u32, u32)>,
}

fn cells_of(
    t: &Tree,
    r: usize,
    bold: &HashSet<String>,
    budget: &mut Budget,
) -> Result<Vec<RowCell>, OdfError> {
    let mut out = Vec::new();
    let mut col = 0u64;
    for c in t.elements(r) {
        let name = t.node(c).name.as_str();
        if name != "table-cell" && name != "covered-table-cell" {
            continue;
        }
        if col >= MAX_COLUMNS {
            break;
        }
        let repeat = count(t.attr(c, "number-columns-repeated"));
        let shown = t
            .elements(c)
            .filter(|&p| t.node(p).name == "p")
            .map(|p| t.all_text(p))
            .collect::<Vec<_>>()
            .join("\n");
        let datum = datum(t, c);
        let text = if shown.trim().is_empty() {
            datum.as_ref().map(|d| d.value.clone()).unwrap_or_default()
        } else {
            shown
        };
        if text.trim().is_empty() || name == "covered-table-cell" {
            col = col.saturating_add(repeat);
            continue;
        }
        let span = |a: &str| {
            t.attr(c, a)
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1)
                .clamp(1, 1 << 14)
        };
        let (cs, rs) = (span("number-columns-spanned"), span("number-rows-spanned"));
        let is_bold = t.attr(c, "style-name").is_some_and(|s| bold.contains(s));
        for _ in 0..repeat {
            if col >= MAX_COLUMNS {
                break;
            }
            spend(budget, &text)?;
            out.push(RowCell {
                cell: GridCell {
                    row: 0,
                    col: col as u32,
                    text: text.clone(),
                    datum: datum.clone(),
                    bold: is_bold,
                },
                span: (cs > 1 || rs > 1).then_some((cs, rs)),
            });
            col += 1;
            // A cell repeated across a row is as many cells, within the
            // sheet's bound on them.
            if out.len() > MAX_CELLS {
                break;
            }
        }
    }
    Ok(out)
}

/// One cell and its text, spent before they are made.
fn spend(budget: &mut Budget, text: &str) -> Result<(), OdfError> {
    let too_large = |e: TooLarge| OdfError::TooLarge(e.to_string());
    budget.spend_cells(1).map_err(too_large)?;
    budget.spend_text(text).map_err(too_large)
}

/// A `number-*-repeated` count; 1 where absent or not a number.
fn count(v: Option<&str>) -> u64 {
    v.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1)
        .max(1)
}

/// A sheet's rows in document order, through header rows and groups.
fn rows_of(t: &Tree, n: usize, header: bool, out: &mut Vec<(usize, bool)>) {
    for c in t.elements(n) {
        match t.node(c).name.as_str() {
            "table-row" => out.push((c, header)),
            "table-header-rows" => rows_of(t, c, true, out),
            "table-rows" | "table-row-group" => rows_of(t, c, header, out),
            _ => {}
        }
    }
}

/// What a cell stores, typed, by its value type.
fn datum(t: &Tree, c: usize) -> Option<Datum> {
    match t.attr(c, "value-type")? {
        "float" | "percentage" | "currency" => {
            let v = t.attr(c, "value")?.trim().parse::<f64>().ok()?;
            v.is_finite()
                .then(|| Datum::new("xsd:decimal", format!("{v}")))
        }
        "boolean" => match t.attr(c, "boolean-value")? {
            "true" => Some(Datum::new("xsd:boolean", "true")),
            "false" => Some(Datum::new("xsd:boolean", "false")),
            _ => None,
        },
        "date" => fluree_doc_model::xsd_temporal(t.attr(c, "date-value")?)
            .map(|(ty, v)| Datum::new(ty, v)),
        "time" => duration_time(t.attr(c, "time-value")?).map(|v| Datum::new("xsd:time", v)),
        _ => None,
    }
}

/// `PT13H05M30S` as `13:05:30`, for a time within a day.
fn duration_time(d: &str) -> Option<String> {
    let rest = d.trim().strip_prefix("PT")?;
    let (mut h, mut m, mut s) = (0f64, 0f64, 0f64);
    let mut num = String::new();
    for ch in rest.chars() {
        match ch {
            'H' => h = std::mem::take(&mut num).parse().ok()?,
            'M' => m = std::mem::take(&mut num).parse().ok()?,
            'S' => s = std::mem::take(&mut num).parse().ok()?,
            c => num.push(c),
        }
    }
    let secs = (h * 3600.0 + m * 60.0 + s).round() as i64;
    (0..86_400)
        .contains(&secs)
        .then(|| format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60))
}

/// The cell styles whose text is bold.
fn bold_styles(t: &Tree) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(auto) = t.find(0, "automatic-styles") else {
        return out;
    };
    for s in t.elements(auto) {
        let bold = t
            .find(s, "text-properties")
            .and_then(|p| t.attr(p, "font-weight"))
            .is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
        if let (true, Some(name)) = (bold, t.attr(s, "name")) {
            out.insert(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::tests::package;
    use crate::{parse, OdfError};

    fn sheet(rows: &str) -> Vec<u8> {
        package(
            "spreadsheet",
            &format!("<office:spreadsheet><table:table table:name=\"S\">{rows}</table:table></office:spreadsheet>"),
            &[],
        )
    }

    #[test]
    fn a_repeated_cell_counts_its_text_each_time() {
        // A 96 KiB cell repeated 3,000 times across one row: 288 MiB of
        // copies for the row, and as much again for the sheet, from a few
        // hundred bytes compressed, within the bound on cells.
        let big = "y".repeat(96 << 10);
        let row = format!(
            "<table:table-row><table:table-cell table:number-columns-repeated=\"3000\"><text:p>{big}</text:p></table:table-cell></table:table-row>"
        );
        assert!(
            matches!(parse(&sheet(&row)), Err(OdfError::TooLarge(ref m)) if m.contains("text")),
            "{:?}",
            parse(&sheet(&row)).map(|e| e.len())
        );
        // The same cell a few times reads.
        let row = row.replace("3000", "3");
        assert!(parse(&sheet(&row)).is_ok());
    }
}
