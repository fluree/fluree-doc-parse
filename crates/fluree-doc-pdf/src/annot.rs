//! What a PDF carries beside its pages' content: the notes a reader left on
//! them (text, free-text and markup annotations), and the files it embeds.
//!
//! Neither is drawn by the content stream, so the glyph pass never sees
//! them. A reviewer's sticky note, the comment on a highlight, a free-text
//! box typed onto a form: each is the page's text to a person reading it in
//! a viewer, and lost to anything reading glyphs. An embedded file is a
//! document of its own, carried inside this one as an e-mail carries an
//! attachment: it is described and handed back as bytes, not read here.

use crate::geom::BBox;
use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{Attachment, Element};
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, ObjRef, Stream, String as PdfString};
use hayro_syntax::Pdf;
use std::collections::{HashMap, HashSet};
use std::io::Read;

/// A note on a page: the kind of annotation, where it sits, who wrote it,
/// and what it says.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub page: usize,
    /// The annotation's subtype: `Text` (a sticky note), `FreeText`, or a
    /// markup (`Highlight`, `Underline`, `StrikeOut`, `Squiggly`).
    pub subtype: String,
    /// Its rectangle, top-left origin, as a link's.
    pub bbox: BBox,
    /// Who wrote it (`/T`), where it says.
    pub author: Option<String>,
    /// What it says (`/Contents`).
    pub contents: String,
}

/// The annotation subtypes whose `/Contents` is a note a person wrote.
const NOTES: [&[u8]; 6] = [
    b"Text",
    b"FreeText",
    b"Highlight",
    b"Underline",
    b"StrikeOut",
    b"Squiggly",
];

/// Cap on notes read from one file, as on links: `/Annots` is an arbitrary
/// array a hostile file may make enormous.
const MAX_ANNOTATIONS: usize = 20_000;

/// The most entries of the pages' `/Annots` arrays looked at, notes or not,
/// across the document: past them, the notes found so far are all there are.
const MAX_ANNOTATION_ENTRIES: usize = 200_000;

/// The most characters of one note kept.
const MAX_NOTE_CHARS: usize = 64 << 10;

/// The most characters of an embedded file's name kept.
const MAX_NAME_CHARS: usize = 1 << 10;

/// Every note with something written in it, in page order, then in the
/// order the page lists them; each annotation once, however many pages or
/// times list it, and each page's `/Annots` array once, however many pages
/// share it. Each note is spent from `budget` (an element and its text)
/// before it is kept: past it, the document is too large.
pub fn annotations(pdf: &Pdf, budget: &mut Budget) -> Result<Vec<Annotation>, TooLarge> {
    let mut out = Vec::new();
    let mut texts = Texts::default();
    let mut seen_arrays = HashSet::new();
    let mut seen = HashSet::new();
    let mut entries = 0usize;
    for (page, p) in pdf.pages().iter().enumerate() {
        if let Some(r) = p.raw().get_ref(b"Annots") {
            if !seen_arrays.insert(r) {
                continue;
            }
        }
        let Some(annots) = p.raw().get::<Array<'_>>(b"Annots") else {
            continue;
        };
        let (_, height) = p.render_dimensions();
        for entry in crate::array_entries(&annots) {
            entries += 1;
            if out.len() >= MAX_ANNOTATIONS || entries > MAX_ANNOTATION_ENTRIES {
                return Ok(out);
            }
            let a = match entry {
                MaybeRef::Ref(r) if !seen.insert(r) => continue,
                MaybeRef::Ref(r) => pdf.xref().get::<Dict<'_>>(r.into()),
                MaybeRef::NotRef(o) => Dict::try_from(o).ok(),
            };
            let Some(a) = a else {
                continue;
            };
            let Some(subtype) = a.get::<Name<'_>>(b"Subtype") else {
                continue;
            };
            if !NOTES.contains(&subtype.as_ref()) {
                continue;
            }
            let Some(bbox) = crate::link::rect_of(&a, height as f64) else {
                continue;
            };
            let contents = texts.get(&a, b"Contents", MAX_NOTE_CHARS);
            if contents.is_empty() {
                continue;
            }
            let author = Some(texts.get(&a, b"T", MAX_NOTE_CHARS)).filter(|t| !t.is_empty());
            budget.spend_elements(1)?;
            budget.spend_text(&contents)?;
            budget.spend_text(author.as_deref().unwrap_or_default())?;
            out.push(Annotation {
                page,
                subtype: String::from_utf8_lossy(subtype.as_ref()).into_owned(),
                bbox,
                author,
                contents,
            });
        }
    }
    Ok(out)
}

/// The text strings read so far that are objects of their own, by their
/// reference: one a thousand notes all name is parsed and decoded once.
#[derive(Default)]
struct Texts {
    by_ref: HashMap<ObjRef, String>,
}

impl Texts {
    /// A text string entry, decoded, its runs of whitespace one space, at
    /// most `max` characters; empty where there is none.
    fn get(&mut self, d: &Dict<'_>, key: &[u8], max: usize) -> String {
        let Some(r) = d.get_ref(key) else {
            return text(d, key, max).unwrap_or_default();
        };
        self.by_ref
            .entry(r)
            .or_insert_with(|| text(d, key, max).unwrap_or_default())
            .clone()
    }
}

/// A text string entry, decoded, its runs of whitespace one space, at most
/// `max` characters: no more of its bytes are decoded than make them.
fn text(d: &Dict<'_>, key: &[u8], max: usize) -> Option<String> {
    let bytes = d.get::<PdfString>(key)?;
    let bytes = bytes.as_bytes();
    // A byte-order mark, then at most four bytes a character.
    let bytes = &bytes[..bytes.len().min(3 + 4 * max)];
    let s = crate::outline::decode_text_string(bytes);
    let mut out = String::new();
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() >= 4 * max {
            break;
        }
    }
    Some(out.chars().take(max).collect())
}

/// The notes as elements: a paragraph each, where it sits on its page,
/// `evidence` `annotation`, its text opening with its author
/// (`Sam Example: Check this total`) as a transcript turn opens with its
/// speaker.
pub fn elements(annotations: &[Annotation]) -> Vec<Element> {
    annotations
        .iter()
        .map(|a| {
            let text = match &a.author {
                Some(who) => format!("{who}: {}", a.contents),
                None => a.contents.clone(),
            };
            Element {
                page: a.page,
                bbox: Some(a.bbox),
                ..Element::new("doco:Paragraph", text, "rust", "annotation")
            }
        })
        .collect()
}

/// Put each note after its page's last element, so a page reads its own
/// text first and the notes on it after; a page with no elements takes them
/// where it would fall. Each is named `annot-<n>`, so the elements around it
/// keep their ids, and a table's `continues` the id it names.
///
/// Notes falling after the same element keep their pages' order, then
/// their own. Placed in one pass, not inserted one by one: a document of
/// thousands of notes and elements costs a sort, not their product.
pub fn place(elements: &mut Vec<Element>, notes: Vec<Element>) {
    if notes.is_empty() {
        return;
    }
    // After which element a note on each page falls: the last on that page
    // or before it.
    let mut last_on: std::collections::BTreeMap<usize, usize> = Default::default();
    for (i, e) in elements.iter().enumerate() {
        last_on.insert(e.page, i + 1);
    }
    let mut upto = 0usize;
    for at in last_on.values_mut() {
        upto = upto.max(*at);
        *at = upto;
    }
    let mut placed: Vec<(usize, usize, usize, Element)> = notes
        .into_iter()
        .enumerate()
        .map(|(i, mut n)| {
            n.id = format!("annot-{:05}", i + 1);
            let at = last_on
                .range(..=n.page)
                .next_back()
                .map_or(0, |(_, at)| *at);
            (at, n.page, i, n)
        })
        .collect();
    placed.sort_by_key(|(at, page, i, _)| (*at, *page, *i));
    let mut out = Vec::with_capacity(elements.len() + placed.len());
    let mut notes = placed.into_iter().peekable();
    for (i, e) in std::mem::take(elements).into_iter().enumerate() {
        while let Some((_, _, _, n)) = notes.next_if(|(at, ..)| *at == i) {
            out.push(n);
        }
        out.push(e);
    }
    out.extend(notes.map(|(_, _, _, n)| n));
    *elements = out;
}

/// A file a PDF embeds: described as an e-mail attachment is, with its
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedFile {
    pub info: Attachment,
    pub bytes: Vec<u8>,
}

/// An embedded file that could not be taken out, by its name, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub name: Option<String>,
    /// `TooLarge` (past [`MAX_FILE_BYTES`] decoded, or past
    /// [`MAX_TOTAL_BYTES`] or [`MAX_DECODED_BYTES`] for the document),
    /// `UnsupportedFilter` (encoded otherwise than with Flate, or not at
    /// all), `TooMany` (past [`MAX_FILES`]) or `Failed` (it did not decode).
    pub reason: &'static str,
}

/// The most files taken out of one document.
pub const MAX_FILES: usize = 1_000;
/// The most bytes one embedded file is decoded to: 64 MiB. This and the
/// two bounds below are a sixteenth built for the fuzz targets, as the
/// budget is ([`fluree_doc_model::budget::scaled`]).
pub const MAX_FILE_BYTES: u64 = fluree_doc_model::budget::scaled(64 << 20);
/// The most bytes all of a document's embedded files are decoded to: 256 MiB.
pub const MAX_TOTAL_BYTES: u64 = fluree_doc_model::budget::scaled(256 << 20);
/// The most bytes decoding a document's embedded files may inflate, the
/// files kept, those refused and those found again alike: 512 MiB. A file
/// refused past its bound has cost its decoding all the same.
pub const MAX_DECODED_BYTES: u64 = fluree_doc_model::budget::scaled(512 << 20);
/// How deep the embedded-files name tree is followed.
const MAX_TREE_DEPTH: usize = 32;

/// The files the document embeds: those its catalog's `/EmbeddedFiles` name
/// tree lists, then those `FileAttachment` annotations carry, in page order,
/// each once by its bytes' SHA-256. A file's stream is decoded within the
/// bounds above, and once however many specifications name it; one that
/// cannot be is listed with why, once.
pub fn embedded_files(pdf: &Pdf) -> (Vec<EmbeddedFile>, Vec<SkippedFile>) {
    embedded_files_within(pdf, &LIMITS)
}

/// The bounds [`embedded_files`] decodes within.
struct Limits {
    files: usize,
    file_bytes: u64,
    total_bytes: u64,
    decoded_bytes: u64,
}

const LIMITS: Limits = Limits {
    files: MAX_FILES,
    file_bytes: MAX_FILE_BYTES,
    total_bytes: MAX_TOTAL_BYTES,
    decoded_bytes: MAX_DECODED_BYTES,
};

fn embedded_files_within(pdf: &Pdf, limits: &Limits) -> (Vec<EmbeddedFile>, Vec<SkippedFile>) {
    let mut specs: Vec<Dict<'_>> = Vec::new();
    let xref = pdf.xref();
    if let Some(tree) = xref
        .get::<Dict<'_>>(xref.root_id())
        .and_then(|c| c.get::<Dict<'_>>(b"Names"))
        .and_then(|n| n.get::<Dict<'_>>(b"EmbeddedFiles"))
    {
        name_tree(&tree, 0, &mut specs);
    }
    for p in pdf.pages().iter() {
        let Some(annots) = p.raw().get::<Array<'_>>(b"Annots") else {
            continue;
        };
        for a in annots.iter::<Dict<'_>>() {
            if specs.len() > limits.files {
                break;
            }
            let attachment = a
                .get::<Name<'_>>(b"Subtype")
                .is_some_and(|n| n.as_ref() == b"FileAttachment");
            if let Some(fs) = attachment.then(|| a.get::<Dict<'_>>(b"FS")).flatten() {
                specs.push(fs);
            }
        }
    }
    let (mut files, mut skipped) = (Vec::<EmbeddedFile>::new(), Vec::new());
    let mut total = 0u64;
    // What decoding may still inflate, files refused included.
    let mut decodable = limits.decoded_bytes;
    // The streams decoded, or refused, already: a stream every
    // specification names is one file, and one refusal.
    let mut seen = HashSet::new();
    let mut kept = HashSet::new();
    let mut names = Texts::default();
    for (i, spec) in specs.iter().enumerate() {
        let name = ["UF", "F"]
            .iter()
            .map(|k| names.get(spec, k.as_bytes(), MAX_NAME_CHARS))
            .find(|n| !n.is_empty());
        if i >= limits.files {
            skipped.push(SkippedFile {
                name,
                reason: "TooMany",
            });
            continue;
        }
        let Some(stream) = spec.get::<Dict<'_>>(b"EF").and_then(|ef| {
            ef.get::<Stream<'_>>(b"UF")
                .or_else(|| ef.get::<Stream<'_>>(b"F"))
        }) else {
            continue;
        };
        let id = stream.obj_id();
        // A crafted stream may have no object number: not remembered, but
        // its decoding still spends the budget.
        if id.obj_number != 0 && !seen.insert(id) {
            continue;
        }
        let room = limits
            .file_bytes
            .min(limits.total_bytes - total)
            .min(decodable);
        let bytes = match decode(&stream, room, &mut decodable) {
            Ok(b) => b,
            Err(reason) => {
                skipped.push(SkippedFile { name, reason });
                continue;
            }
        };
        let sha256 = fluree_doc_model::sha256_hex(&bytes);
        if !kept.insert(sha256.clone()) {
            continue;
        }
        total += bytes.len() as u64;
        let content_type = stream
            .dict()
            .get::<Name<'_>>(b"Subtype")
            .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
            .filter(|t| t.contains('/'))
            .unwrap_or_else(|| "application/octet-stream".into());
        files.push(EmbeddedFile {
            info: Attachment {
                filename: name,
                content_type,
                size: bytes.len(),
                sha256,
                inline: false,
            },
            bytes,
        });
    }
    (files, skipped)
}

/// The file specifications a name tree node lists, and its kids'.
fn name_tree<'a>(node: &Dict<'a>, depth: usize, out: &mut Vec<Dict<'a>>) {
    if depth > MAX_TREE_DEPTH || out.len() > MAX_FILES {
        return;
    }
    if let Some(names) = node.get::<Array<'_>>(b"Names") {
        // Key, value, key, value: the values are the specifications.
        for (i, v) in names.iter::<hayro_syntax::object::Object<'_>>().enumerate() {
            if i % 2 == 1 {
                if let Ok(d) = Dict::try_from(v) {
                    out.push(d);
                }
            }
            if out.len() > MAX_FILES {
                return;
            }
        }
    }
    if let Some(kids) = node.get::<Array<'_>>(b"Kids") {
        for kid in kids.iter::<Dict<'_>>() {
            name_tree(&kid, depth + 1, out);
        }
    }
}

/// An embedded file's stream decoded, at most `max` bytes, each byte
/// inflated (kept or not) spent from `decodable`. Only Flate (the filter
/// writers use for embedded files) is decoded, here, against the bound: the
/// parser's own decoders inflate without one.
fn decode(stream: &Stream<'_>, max: u64, decodable: &mut u64) -> Result<Vec<u8>, &'static str> {
    use hayro_syntax::Filter;
    let filters = stream.filters();
    let predicted = stream.dict().get::<Dict<'_>>(b"DecodeParms").is_some()
        || stream.dict().get::<Array<'_>>(b"DecodeParms").is_some();
    if predicted || filters.iter().any(|f| *f != Filter::FlateDecode) {
        return Err("UnsupportedFilter");
    }
    let mut data = stream.raw_data().into_owned();
    for _ in filters.iter() {
        let mut out = Vec::new();
        let read = flate2::read::ZlibDecoder::new(data.as_slice())
            .take(max + 1)
            .read_to_end(&mut out);
        *decodable = decodable.saturating_sub(out.len() as u64);
        read.map_err(|_| "Failed")?;
        if out.len() as u64 > max {
            return Err("TooLarge");
        }
        data = out;
    }
    // Not encoded at all: as large as it is written.
    if data.len() as u64 > max {
        return Err("TooLarge");
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A PDF of these objects, numbered from 1, its catalog first.
    fn objects_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
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

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        [
            format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes(),
            data.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// A page with a sticky note, a highlight's comment, a link, a note
    /// with nothing in it, and a file attachment; the catalog embedding a
    /// CSV, the same CSV again under another name, a bomb and a file under
    /// a filter not decoded here.
    fn annotated(bomb: &[u8]) -> Vec<u8> {
        let csv = b"Customer,Amount\nAlex Sample,12.50\n";
        objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Kids [5 0 R] >> >> >>"
                .to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Annots [\
               << /Type /Annot /Subtype /Text /Rect [100 700 120 720] /T (Sam Example) /Contents (Check this   total) >>\
               << /Type /Annot /Subtype /Highlight /Rect [50 600 300 615] /Contents <FEFF00C9007400E9> >>\
               << /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Contents (not a note) >>\
               << /Type /Annot /Subtype /FreeText /Rect [0 0 10 10] >>\
               << /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS << /Type /Filespec /F (photo.png) /EF << /F 10 0 R >> >> >>\
               ] >>"
                .to_vec(),
            stream("", b""),
            b"<< /Names [(a) 6 0 R (b) 7 0 R] /Kids [11 0 R] >>".to_vec(),
            b"<< /Type /Filespec /F (orders.csv) /UF (orders.csv) /EF << /F 8 0 R >> >>".to_vec(),
            b"<< /Type /Filespec /F (copy.csv) /EF << /F 9 0 R >> >>".to_vec(),
            stream("/Type /EmbeddedFile /Subtype /text#2Fcsv /Filter /FlateDecode", &zlib(csv)),
            stream("/Type /EmbeddedFile", csv),
            stream("/Type /EmbeddedFile /Subtype /image#2Fpng", b"\x89PNG fake"),
            b"<< /Names [(c) 12 0 R (d) 13 0 R] >>".to_vec(),
            b"<< /Type /Filespec /F (bomb.bin) /EF << /F 14 0 R >> >>".to_vec(),
            b"<< /Type /Filespec /F (old.bin) /EF << /F 15 0 R >> >>".to_vec(),
            stream("/Type /EmbeddedFile /Filter /FlateDecode", bomb),
            stream("/Type /EmbeddedFile /Filter /LZWDecode", b"\x80\x0b\x60\x50\x22\x0c\x0c\x85\x01"),
        ])
    }

    #[test]
    fn an_annotation_list_that_does_not_read_is_read_up_to_where_it_stops() {
        // A lone sign skips as a number but does not read as one: the
        // array's own iterator panicked on it. The note before it is read.
        let pdf = objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R + ] >>".to_vec(),
            b"<< /Type /Annot /Subtype /Text /Rect [10 10 50 50] /Contents (Kept) >>".to_vec(),
        ]);
        let pdf = Pdf::new(std::sync::Arc::new(pdf)).unwrap();
        let notes = annotations(&pdf, &mut Budget::default()).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].contents, "Kept");
        let pdf = Pdf::new(std::sync::Arc::new(objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [+ ] >>".to_vec(),
        ])))
        .unwrap();
        assert!(annotations(&pdf, &mut Budget::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn notes_with_something_written_are_read_with_their_author() {
        let pdf = Pdf::new(std::sync::Arc::new(annotated(&zlib(b"x")))).unwrap();
        let notes = annotations(&pdf, &mut Budget::default()).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(notes[0].subtype, "Text");
        assert_eq!(notes[0].author.as_deref(), Some("Sam Example"));
        assert_eq!(notes[0].contents, "Check this total");
        // Top-left origin, as every box.
        assert_eq!(notes[0].bbox.y0, 80.0);
        assert_eq!(notes[1].subtype, "Highlight");
        assert_eq!(notes[1].contents, "Été");
        let els = elements(&notes);
        assert_eq!(els[0].text, "Sam Example: Check this total");
        assert_eq!(els[0].evidence, "annotation");
        assert_eq!(els[1].text, "Été");
    }

    #[test]
    fn a_note_listed_again_is_read_once_and_notes_spend_the_budget() {
        // One note listed a thousand times on each of two pages sharing one
        // `/Annots` array, a second note listed once, a third whose text,
        // an object of its own, the second's also names, and one written
        // in the array, read once with it.
        let long = "word ".repeat(MAX_NOTE_CHARS);
        let pdf = objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots 5 0 R >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots 5 0 R >>".to_vec(),
            [
                b"[".to_vec(),
                b"6 0 R ".repeat(1000),
                b"7 0 R 8 0 R << /Subtype /Text /Rect [0 0 10 10] /Contents (Inline) >>]".to_vec(),
            ]
            .concat(),
            b"<< /Subtype /Text /Rect [0 0 10 10] /T (Sam Example) /Contents (Check) >>".to_vec(),
            b"<< /Subtype /Text /Rect [0 0 10 10] /Contents 9 0 R >>".to_vec(),
            b"<< /Subtype /FreeText /Rect [0 0 10 10] /Contents 9 0 R >>".to_vec(),
            format!("({long})").into_bytes(),
        ]);
        let pdf = Pdf::new(std::sync::Arc::new(pdf)).unwrap();
        let notes = annotations(&pdf, &mut Budget::default()).unwrap();
        let read: Vec<_> = notes
            .iter()
            .map(|n| (n.page, n.subtype.as_str(), n.contents.chars().count()))
            .collect();
        assert_eq!(
            read,
            [
                (0, "Text", 5),
                (0, "Text", MAX_NOTE_CHARS),
                (0, "FreeText", MAX_NOTE_CHARS),
                (0, "Text", 6)
            ]
        );
        // Each note an element and its text, spent before it is kept.
        let need = 4 * fluree_doc_model::budget::ELEMENT_BYTES
            + fluree_doc_model::budget::TEXT_BYTES * (5 + 11 + 2 * MAX_NOTE_CHARS as u64 + 6);
        assert!(annotations(&pdf, &mut Budget::new(need, 0)).is_ok());
        assert!(annotations(&pdf, &mut Budget::new(need - 1, 0)).is_err());
    }

    #[test]
    fn a_note_on_a_scan_leaves_it_unread() {
        let doc = crate::image::page_of(1200, 1600, false);
        let note = Annotation {
            page: 0,
            subtype: "Text".into(),
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 10.0,
            },
            author: Some("Sam Example".into()),
            contents: "Please check the totals on this scanned page again".into(),
        };
        let els = elements(&[note]);
        assert_eq!(crate::unread_pages(&doc, &els).len(), 1);
    }

    #[test]
    fn notes_go_after_their_pages_text() {
        let el = |page, text: &str| Element {
            page,
            ..Element::new("doco:Paragraph", text, "rust", "layout")
        };
        let mut els = vec![el(0, "a"), el(0, "b"), el(2, "c")];
        place(&mut els, vec![el(0, "n0"), el(1, "n1"), el(3, "n3")]);
        let order: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(order, ["a", "b", "n0", "n1", "c", "n3"]);
        assert_eq!(els[2].id, "annot-00001");
    }

    #[test]
    fn notes_placed_in_one_pass_fall_as_inserted_one_by_one_would() {
        let el = |page, text: &str| Element {
            page,
            ..Element::new("doco:Paragraph", text, "rust", "layout")
        };
        // Elements out of page order, notes too, and notes before any
        // element and past the last page.
        let mut els = vec![el(1, "a"), el(0, "b"), el(3, "c"), el(2, "d")];
        let notes = vec![
            el(2, "n2"),
            el(0, "n0"),
            el(5, "n5"),
            el(2, "m2"),
            el(3, "n3"),
        ];
        let mut one_by_one = els.clone();
        for n in notes.clone() {
            let at = one_by_one
                .iter()
                .rposition(|e| e.page <= n.page)
                .map_or(0, |i| i + 1);
            one_by_one.insert(at, n);
        }
        place(&mut els, notes);
        let order = |v: &[Element]| v.iter().map(|e| e.text.clone()).collect::<Vec<_>>();
        assert_eq!(order(&els), order(&one_by_one));
        let mut empty = Vec::new();
        place(&mut empty, vec![el(4, "x"), el(1, "y")]);
        assert_eq!(order(&empty), ["y", "x"]);
    }

    #[test]
    fn a_stream_named_again_is_decoded_once_and_refused_once() {
        // Five specifications of one stream that inflates past the file's
        // bound: one refusal, one decoding.
        let bomb = zlib(&vec![0u8; 4096]);
        let pdf = objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 4 0 R >> >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>".to_vec(),
            b"<< /Names [(a) 5 0 R (b) 5 0 R (c) 5 0 R (d) 6 0 R (e) 6 0 R] >>".to_vec(),
            b"<< /Type /Filespec /F (bomb.bin) /EF << /F 7 0 R >> >>".to_vec(),
            b"<< /Type /Filespec /F (again.bin) /EF << /F 7 0 R >> >>".to_vec(),
            stream("/Type /EmbeddedFile /Filter /FlateDecode", &bomb),
        ]);
        let pdf = Pdf::new(std::sync::Arc::new(pdf)).unwrap();
        let limits = Limits {
            file_bytes: 1000,
            ..LIMITS
        };
        let (files, skipped) = embedded_files_within(&pdf, &limits);
        assert!(files.is_empty());
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        // What was refused counts toward what decoding may inflate: two
        // streams refused past the file's bound spend the document's.
        let pdf = objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 4 0 R >> >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>".to_vec(),
            b"<< /Names [(a) 5 0 R (b) 6 0 R (c) 7 0 R] >>".to_vec(),
            b"<< /Type /Filespec /F (one.bin) /EF << /F 8 0 R >> >>".to_vec(),
            b"<< /Type /Filespec /F (two.bin) /EF << /F 9 0 R >> >>".to_vec(),
            b"<< /Type /Filespec /F (small.txt) /EF << /F 10 0 R >> >>".to_vec(),
            stream("/Type /EmbeddedFile /Filter /FlateDecode", &bomb),
            stream(
                "/Type /EmbeddedFile /Filter /FlateDecode",
                &zlib(&vec![1u8; 4096]),
            ),
            stream("/Type /EmbeddedFile /Filter /FlateDecode", &zlib(b"small")),
        ]);
        let pdf = Pdf::new(std::sync::Arc::new(pdf)).unwrap();
        let limits = Limits {
            file_bytes: 1000,
            decoded_bytes: 2002,
            ..LIMITS
        };
        let (files, skipped) = embedded_files_within(&pdf, &limits);
        assert!(files.is_empty(), "{files:?}");
        let why: Vec<_> = skipped
            .iter()
            .map(|s| (s.name.as_deref(), s.reason))
            .collect();
        assert_eq!(
            why,
            [
                (Some("one.bin"), "TooLarge"),
                (Some("two.bin"), "TooLarge"),
                (Some("small.txt"), "TooLarge")
            ]
        );
    }

    #[test]
    fn a_stream_is_inflated_no_further_than_its_bound() {
        // A stream whose checksum, at its very end, is wrong: inflated
        // whole, it fails; inflated to its bound and no further, it is too
        // large, and the end is never reached.
        let mut z = zlib(&vec![0u8; 1 << 20]);
        let n = z.len();
        z[n - 4..].copy_from_slice(&[0, 0, 0, 0]);
        let pdf = objects_pdf(&[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>".to_vec(),
            stream("/Type /EmbeddedFile /Filter /FlateDecode", &z),
        ]);
        let pdf = Pdf::new(std::sync::Arc::new(pdf)).unwrap();
        let s = pdf
            .xref()
            .get::<Stream<'_>>(hayro_syntax::object::ObjectIdentifier::new(4, 0))
            .unwrap();
        let mut decodable = u64::MAX;
        assert_eq!(decode(&s, 1000, &mut decodable), Err("TooLarge"));
        assert_eq!(u64::MAX - decodable, 1001);
        assert_eq!(decode(&s, 2 << 20, &mut decodable), Err("Failed"));
    }

    #[test]
    fn embedded_files_come_out_once_each_within_bounds() {
        // Inflates past the per-file bound.
        let bomb = zlib(&vec![0u8; (MAX_FILE_BYTES + 1) as usize]);
        let pdf = Pdf::new(std::sync::Arc::new(annotated(&bomb))).unwrap();
        let (files, skipped) = embedded_files(&pdf);
        let names: Vec<_> = files.iter().map(|f| f.info.filename.as_deref()).collect();
        // `copy.csv` holds the same bytes as `orders.csv`: one file.
        assert_eq!(names, [Some("orders.csv"), Some("photo.png")]);
        let csv = &files[0];
        assert_eq!(csv.bytes, b"Customer,Amount\nAlex Sample,12.50\n");
        assert_eq!(csv.info.content_type, "text/csv");
        assert_eq!(csv.info.size, csv.bytes.len());
        assert_eq!(csv.info.sha256, fluree_doc_model::sha256_hex(&csv.bytes));
        assert_eq!(files[1].info.content_type, "image/png");
        let why: Vec<_> = skipped
            .iter()
            .map(|s| (s.name.as_deref(), s.reason))
            .collect();
        assert_eq!(
            why,
            [
                (Some("bomb.bin"), "TooLarge"),
                (Some("old.bin"), "UnsupportedFilter")
            ]
        );
    }
}
