//! Synthetic PDFs shared by the integration tests.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

/// A 600 × 120 px grey image of two lines of text, drawn by rendering a text
/// PDF: what a screenshot pasted into a page holds.
pub fn capture() -> (u32, u32, Vec<u8>) {
    let text = pdf(
        &[(150.0, 30.0)],
        "BT /F1 12 Tf 4 17 Td (Account 4417 ref) Tj ET\nBT /F1 12 Tf 4 4 Td (Date 14 MAR 1900) Tj ET",
        "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>",
        &[],
    );
    let raw = hayro_syntax::Pdf::new(std::sync::Arc::new(text)).unwrap();
    let mut out = (0, 0, Vec::new());
    fluree_doc_pdf::escalate::for_each_crop_scaled(
        &raw,
        &vec![(0, None)],
        |_, _| 4.0,
        |c| {
            let gray = c.pixels.data.chunks_exact(4).map(|p| p[0]).collect();
            out = (c.pixels.width, c.pixels.height, gray);
        },
    );
    out
}

/// A one-page PDF of the given size, content, resources and images, the
/// images named in an `/XObject` dictionary.
pub fn pdf(
    size: &[(f64, f64)],
    content: &str,
    fonts: &str,
    images: &[(String, String, Vec<u8>)],
) -> Vec<u8> {
    let (w, h) = size[0];
    let xobjects: String = images
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| format!("/{name} {} 0 R ", 5 + i))
        .collect();
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << {fonts} \
             /XObject << {xobjects}>> >> /Contents 4 0 R >>"
        )
        .into_bytes(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
        .into_bytes(),
    ];
    for (_, dict, data) in images {
        objects.push(
            [
                format!("<< /Type /XObject /Subtype /Image {dict} >>\nstream\n").into_bytes(),
                data.clone(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        );
    }
    objects_pdf(&objects)
}

/// A PDF of these objects, numbered from 1, the first its catalog, with a
/// cross-reference table that finds each.
pub fn objects_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .into_bytes(),
    );
    out
}
