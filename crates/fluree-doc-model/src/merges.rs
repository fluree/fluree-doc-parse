//! Vertical and horizontal cell merges, and the denormalised view.

/// Which cells continue the cell above them, and which rows are one
/// full-width cell — see [`Grid::merges`].
pub struct Merges {
    /// Row-major, `rows * cols`: this cell is part of the same logical cell
    /// as the one directly above.
    pub continues_above: Vec<bool>,
    /// Row-major, `rows * cols`: this cell is part of the same logical cell
    /// as the one directly to its left — the boundary is not drawn across
    /// this row. Column 0 is never a continuation.
    pub continues_left: Vec<bool>,
    /// Per row: the row is a single cell spanning every column — a banner or
    /// sub-header band.
    pub full_width_row: Vec<bool>,
}

/// Denormalise a row-major cell grid so every row stands on its own.
///
/// A merged cell's text wraps into whichever row bands it happens to cross,
/// so the fragments are gathered and then repeated into every row of the
/// span; a full-width band is coalesced into its first cell. The result is a
/// grid where each row carries its full context — what entity extraction
/// needs, where a cell must describe itself.
///
/// This is deliberately *not* applied to [`Element::cells`], which stays as
/// detected. Repeating a spanned value contradicts how a reference encodes a
/// rowspan (one cell, not N copies) and measured TEDS −0.12 when it was;
/// structure-preserving consumers read the same flags and emit real spans
/// instead.
///
/// [`Element::cells`]: crate::document::Element::cells
pub fn denormalize(rows: &mut [Vec<String>], m: &Merges) {
    let n_rows = rows.len();
    if n_rows == 0 {
        return;
    }
    let cols = rows[0].len();
    for c in 0..cols {
        let mut r = 0;
        while r < n_rows {
            let mut end = r + 1;
            while end < n_rows
                && m.continues_above
                    .get(end * cols + c)
                    .copied()
                    .unwrap_or(false)
            {
                end += 1;
            }
            if end > r + 1 {
                let joined = (r..end)
                    .map(|i| rows[i][c].trim())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                for row in rows.iter_mut().take(end).skip(r) {
                    row[c] = joined.clone();
                }
            }
            r = end;
        }
    }
    // Horizontal spans: gather each run of columns the boundary is not drawn
    // between, so a prose row sliced by a nested table's ruling reads whole.
    for (r, row) in rows.iter_mut().enumerate() {
        let mut c = 0;
        while c < cols {
            let mut end = c + 1;
            while end < cols
                && m.continues_left
                    .get(r * cols + end)
                    .copied()
                    .unwrap_or(false)
            {
                end += 1;
            }
            if end > c + 1 {
                let joined = (c..end)
                    .map(|k| row[k].trim())
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                row[c] = joined;
                for cell in row.iter_mut().take(end).skip(c + 1) {
                    cell.clear();
                }
            }
            c = end;
        }
    }
    for (r, row) in rows.iter_mut().enumerate() {
        if !m.full_width_row.get(r).copied().unwrap_or(false) {
            continue;
        }
        let joined = row
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        row.iter_mut().for_each(|c| c.clear());
        row[0] = joined;
    }
}
