//! The PDF outline (bookmark) tree.
//!
//! Author-provided, explicitly hierarchical heading structure sitting in the
//! file. **No engine we benchmarked reads it.** For any PDF carrying
//! bookmarks this is near-ground-truth for heading text and level, which
//! makes it the clearest available lever on heading structure.
//!
//! It is evidence, not gospel: outlines can be stale, partial, or generated
//! from a template. It is used to *confirm and level* headings found visually,
//! never as the sole source.

use hayro_syntax::object::{Array, Dict, Name, Number, ObjRef};
use hayro_syntax::Pdf;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    /// 1-based nesting depth in the bookmark tree.
    pub level: usize,
    /// Where the bookmark lands, when it names a page of this document.
    ///
    /// Title text drifts: a bookmark reads "Introduction" and the page says
    /// "1. Introduction", or a generator writes an abbreviation. And a title
    /// alone cannot separate the fourth "Overview" from the first. The
    /// destination can do both, so it is carried alongside.
    pub page: Option<usize>,
    /// Top of the destination view in render coordinates (top-left origin),
    /// the same space as a block's `bbox`. `None` where the destination names
    /// a page but no position on it.
    pub y: Option<f64>,
}

/// Guards against a malformed or hostile file: `/First`+`/Next` chains are
/// arbitrary object graphs and may contain cycles.
const MAX_ITEMS: usize = 5_000;
const MAX_DEPTH: usize = 12;

/// Read the outline tree, flattened to (title, level) in document order.
/// Empty when the document has no outline.
pub fn extract(pdf: &Pdf) -> Vec<OutlineItem> {
    let xref = pdf.xref();
    let Some(catalog) = xref.get::<Dict>(xref.root_id()) else {
        return Vec::new();
    };
    let Some(outlines) = catalog.get::<Dict>(b"Outlines") else {
        return Vec::new();
    };
    // Page object -> index, and its height, for resolving destinations. A
    // destination's y is measured from the bottom, like everything else in a
    // PDF; a block's bbox is measured from the top.
    let pages = pdf.pages();
    let geometry: HashMap<(i32, i32), (usize, f64)> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let id = ObjRef::from(p.raw().obj_id()?);
            let (_, height) = p.render_dimensions();
            Some(((id.obj_number, id.gen_number), (i, f64::from(height))))
        })
        .collect();
    let mut out = Vec::new();
    if let Some(first) = outlines.get::<Dict>(b"First") {
        walk(first, 1, &geometry, &mut out);
    }
    out
}

/// The page and top position a bookmark points at.
fn destination(
    node: &Dict<'_>,
    geometry: &HashMap<(i32, i32), (usize, f64)>,
) -> (Option<usize>, Option<f64>) {
    let array = node.get::<Array<'_>>(b"Dest").or_else(|| {
        let action = node.get::<Dict<'_>>(b"A")?;
        let is_goto = action
            .get::<Name<'_>>(b"S")
            .is_some_and(|n| n.as_ref() == b"GoTo");
        is_goto.then(|| action.get::<Array<'_>>(b"D")).flatten()
    });
    let Some(array) = array else {
        return (None, None);
    };
    let Some(reference) = array.raw_iter().next().and_then(|o| o.as_obj_ref()) else {
        return (None, None);
    };
    let Some(&(index, height)) = geometry.get(&(reference.obj_number, reference.gen_number)) else {
        return (None, None);
    };
    // `/XYZ left top zoom` is the common form and the only one that states a
    // position; `/Fit` and friends name the page alone.
    let mut rest = array.raw_iter().skip(1);
    let kind = rest.next().and_then(|o| match o {
        hayro_syntax::object::MaybeRef::NotRef(v) => v.clone().into_name(),
        hayro_syntax::object::MaybeRef::Ref(_) => None,
    });
    let y = match kind.as_ref().map(|n| n.as_ref()) {
        Some(b"XYZ") => array.iter::<Number>().nth(3).map(|n| height - n.as_f64()),
        _ => None,
    };
    (Some(index), y)
}

fn walk(
    node: Dict<'_>,
    level: usize,
    geometry: &HashMap<(i32, i32), (usize, f64)>,
    out: &mut Vec<OutlineItem>,
) {
    if level > MAX_DEPTH {
        return;
    }
    let mut cur = Some(node);
    while let Some(n) = cur {
        if out.len() >= MAX_ITEMS {
            return;
        }
        if let Some(raw) = n.get::<hayro_syntax::object::String>(b"Title") {
            let title = decode_text_string(raw.as_ref()).trim().to_string();
            if !title.is_empty() {
                let (page, y) = destination(&n, geometry);
                out.push(OutlineItem {
                    title,
                    level,
                    page,
                    y,
                });
            }
        }
        if let Some(child) = n.get::<Dict>(b"First") {
            walk(child, level + 1, geometry, out);
        }
        cur = n.get::<Dict>(b"Next");
    }
}

/// Decode a PDF text string: UTF-16BE when the BOM is present, otherwise
/// PDFDocEncoding — close enough to Latin-1 for title text.
fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16be_with_bom() {
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]),
            "Hi"
        );
    }

    #[test]
    fn decodes_plain_bytes_as_latin1() {
        assert_eq!(decode_text_string(b"Introduction"), "Introduction");
    }
}
