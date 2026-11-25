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

use hayro_syntax::object::{Array, Dict, Name, Number, ObjRef, Object};
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
    let named = named_destinations(&catalog, &geometry);
    let mut out = Vec::new();
    if let Some(first) = outlines.get::<Dict>(b"First") {
        walk(first, 1, &geometry, &named, &mut out);
    }
    out
}

/// Where a bookmark points, as it may say it: an explicit destination
/// array, or the name of one.
type Place = (Option<usize>, Option<f64>);

/// Most named destinations read from one document.
const MAX_NAMED: usize = 100_000;

/// Named destinations, by name: the `/Dests` name tree under the catalog's
/// `/Names`, and the older `/Dests` dictionary. Layout programs write
/// bookmarks that point at a destination by name
/// (`den24-0031_Layout.indd:Appendix 1:309`) rather than at a page.
fn named_destinations(
    catalog: &Dict<'_>,
    geometry: &HashMap<(i32, i32), (usize, f64)>,
) -> HashMap<Vec<u8>, Place> {
    let mut out = HashMap::new();
    if let Some(tree) = catalog
        .get::<Dict>(b"Names")
        .and_then(|n| n.get::<Dict>(b"Dests"))
    {
        name_tree(&tree, 0, geometry, &mut out);
    }
    if let Some(dests) = catalog.get::<Dict>(b"Dests") {
        for key in dests.keys() {
            if out.len() >= MAX_NAMED {
                break;
            }
            if let Some(value) = dests.get::<Object>(key.as_ref()) {
                out.insert(key.as_ref().to_vec(), place_of(value, geometry));
            }
        }
    }
    out
}

/// A name tree's entries, its `/Names` pairs and those of its `/Kids`.
fn name_tree(
    node: &Dict<'_>,
    depth: usize,
    geometry: &HashMap<(i32, i32), (usize, f64)>,
    out: &mut HashMap<Vec<u8>, Place>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_NAMED {
        return;
    }
    if let Some(names) = node.get::<Array>(b"Names") {
        let mut items = names.iter::<Object>();
        while let (Some(key), Some(value)) = (items.next(), items.next()) {
            if out.len() >= MAX_NAMED {
                return;
            }
            if let Some(key) = key.into_string() {
                out.insert(key.as_ref().to_vec(), place_of(value, geometry));
            }
        }
    }
    if let Some(kids) = node.get::<Array>(b"Kids") {
        for kid in kids.iter::<Dict>() {
            name_tree(&kid, depth + 1, geometry, out);
        }
    }
}

/// A destination's value: its array, or a dictionary holding it as `/D`.
fn place_of(value: Object<'_>, geometry: &HashMap<(i32, i32), (usize, f64)>) -> Place {
    match value {
        Object::Array(a) => place(&a, geometry),
        Object::Dict(d) => d
            .get::<Array>(b"D")
            .map_or((None, None), |a| place(&a, geometry)),
        _ => (None, None),
    }
}

/// The page and top position a bookmark points at: by its `/Dest` or its
/// go-to action's `/D`, an array or the name of one.
fn destination(
    node: &Dict<'_>,
    geometry: &HashMap<(i32, i32), (usize, f64)>,
    named: &HashMap<Vec<u8>, Place>,
) -> Place {
    let target = node.get::<Object>(b"Dest").or_else(|| {
        let action = node.get::<Dict<'_>>(b"A")?;
        let is_goto = action
            .get::<Name<'_>>(b"S")
            .is_some_and(|n| n.as_ref() == b"GoTo");
        is_goto.then(|| action.get::<Object>(b"D")).flatten()
    });
    match target {
        Some(Object::Array(a)) => place(&a, geometry),
        Some(Object::String(name)) => named.get(name.as_ref()).copied().unwrap_or((None, None)),
        Some(Object::Name(name)) => named.get(name.as_ref()).copied().unwrap_or((None, None)),
        _ => (None, None),
    }
}

/// The page and top position a destination array names.
fn place(array: &Array<'_>, geometry: &HashMap<(i32, i32), (usize, f64)>) -> Place {
    let Some(reference) = crate::array_entries(array)
        .next()
        .and_then(|o| o.as_obj_ref())
    else {
        return (None, None);
    };
    let Some(&(index, height)) = geometry.get(&(reference.obj_number, reference.gen_number)) else {
        return (None, None);
    };
    // `/XYZ left top zoom` is the common form and the only one that states a
    // position; `/Fit` and friends name the page alone.
    let mut rest = crate::array_entries(&array).skip(1);
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
    named: &HashMap<Vec<u8>, Place>,
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
                let (page, y) = destination(&n, geometry, named);
                out.push(OutlineItem {
                    title,
                    level,
                    page,
                    y,
                });
            }
        }
        if let Some(child) = n.get::<Dict>(b"First") {
            walk(child, level + 1, geometry, named, out);
        }
        cur = n.get::<Dict>(b"Next");
    }
}

/// Decode a PDF text string: UTF-16BE or, from PDF 2.0, UTF-8 when its BOM
/// says so, otherwise PDFDocEncoding — close enough to Latin-1 for title
/// text.
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(utf8).into_owned();
    }
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
