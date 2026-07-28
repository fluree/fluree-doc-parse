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

use hayro_syntax::object::Dict;
use hayro_syntax::Pdf;

#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    /// 1-based nesting depth in the bookmark tree.
    pub level: usize,
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
    let mut out = Vec::new();
    if let Some(first) = outlines.get::<Dict>(b"First") {
        walk(first, 1, &mut out);
    }
    out
}

fn walk(node: Dict<'_>, level: usize, out: &mut Vec<OutlineItem>) {
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
                out.push(OutlineItem { title, level });
            }
        }
        if let Some(child) = n.get::<Dict>(b"First") {
            walk(child, level + 1, out);
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
