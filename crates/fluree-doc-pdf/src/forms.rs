//! AcroForm field extraction.
//!
//! Filled-in form values live in widget annotations, not in the content
//! stream, so the glyph pass never sees them: a completed tax form parses as
//! its blank template unless the fields are read here. Field name, type,
//! value and placement come from each page's `/Annots`; inheritable entries
//! (`/FT`, `/T`, `/V`) are resolved through `/Parent` per the spec.

use hayro_syntax::object::{Array, Dict, Name, Number, String as PdfString};
use hayro_syntax::Pdf;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FormField {
    pub page: usize,
    pub name: String,
    /// `Tx` (text), `Btn` (button/checkbox), `Ch` (choice), `Sig` (signature).
    pub kind: String,
    /// The field's value, when set: text content, the selected choice, or a
    /// checkbox's export state (`Off` suppressed as unset).
    pub value: Option<String>,
    /// Placement in render coordinates (top-left origin), matching every
    /// other bbox this library emits.
    pub bbox: crate::geom::BBox,
}

/// Walk an inheritable field entry up the `/Parent` chain. (The resolving
/// trait in hayro-syntax is private, so this is monomorphised per type.)
macro_rules! inherited {
    ($fn_name:ident, $ty:ty) => {
        fn $fn_name<'a>(dict: &Dict<'a>, key: &[u8]) -> Option<$ty> {
            let mut d = dict.clone();
            for _ in 0..16 {
                if let Some(v) = d.get::<$ty>(key) {
                    return Some(v);
                }
                match d.get::<Dict<'a>>(b"Parent") {
                    Some(p) => d = p,
                    None => return None,
                }
            }
            None
        }
    };
}
inherited!(inherited_name, Name<'a>);
inherited!(inherited_string, PdfString<'a>);

/// Decode a PDF text string: UTF-16BE when BOM'd, byte text otherwise.
fn decode_pdf_string(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let units: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(b).to_string()
    }
}

fn string_value(dict: &Dict<'_>) -> Option<String> {
    if let Some(s) = inherited_string(dict, b"V") {
        let v = decode_pdf_string(s.as_bytes());
        return (!v.is_empty()).then_some(v);
    }
    if let Some(n) = inherited_name(dict, b"V") {
        let v = String::from_utf8_lossy(n.as_ref()).to_string();
        return (!v.is_empty() && v != "Off").then_some(v);
    }
    None
}

/// Extract every widget-annotation form field in the document.
pub fn fields(pdf: &Pdf) -> Vec<FormField> {
    let mut out = Vec::new();
    for (pi, page) in pdf.pages().iter().enumerate() {
        let Some(annots) = page.raw().get::<Array<'_>>(b"Annots") else {
            continue;
        };
        // Page height for the top-left origin flip.
        let (_, ph) = page.render_dimensions();
        for a in annots.iter::<Dict<'_>>() {
            let is_widget = a
                .get::<Name<'_>>(b"Subtype")
                .map(|n| n.as_ref() == b"Widget")
                .unwrap_or(false);
            if !is_widget {
                continue;
            }
            let Some(ft) = inherited_name(&a, b"FT") else {
                continue;
            };
            let name = inherited_string(&a, b"T")
                .map(|s| decode_pdf_string(s.as_bytes()))
                .unwrap_or_default();
            let rect = a.get::<Array<'_>>(b"Rect").map(|r| {
                let v: Vec<f64> = r.iter::<Number>().map(|n| n.as_f64()).collect();
                if v.len() == 4 {
                    crate::geom::BBox {
                        x0: v[0].min(v[2]),
                        y0: ph as f64 - v[1].max(v[3]),
                        x1: v[0].max(v[2]),
                        y1: ph as f64 - v[1].min(v[3]),
                    }
                } else {
                    crate::geom::BBox::default()
                }
            });
            out.push(FormField {
                page: pi,
                name,
                kind: String::from_utf8_lossy(ft.as_ref()).to_string(),
                value: string_value(&a),
                bbox: rect.unwrap_or_default(),
            });
        }
    }
    out
}
