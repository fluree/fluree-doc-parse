//! Link annotations — the targets a PDF carries outside its content stream.
//!
//! A hyperlink draws nothing. It is a rectangle in the page's `/Annots` with a
//! URI action behind it, so the glyph pass sees the anchor's words and never
//! the address: `see the filing` extracts as three ordinary words, and the one
//! piece of information that made them worth setting apart is gone. Reading
//! the annotations is the only way to recover it.
//!
//! Two things follow from that, and the second is the reason this module
//! exists at all. Deterministically, the target can ride into every output
//! that has somewhere to put it. And a deeper reader looking at pixels can be
//! *told* the addresses rather than left to infer them — a model shown
//! underlined blue text with no target will supply one, and a plausible
//! invented URL is the worst kind of wrong answer, because nothing downstream
//! can tell it from a real one.

use crate::extract::Page;
use crate::geom::BBox;
use fluree_doc_model::{Element, Link as ElementLink, Target};
use hayro_syntax::object::{Array, Dict, Name, Number, ObjRef, String as PdfString};
use hayro_syntax::Pdf;
use std::collections::HashMap;

/// A link annotation: the rectangle a reader can click, and where it points.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// The page carrying the annotation.
    pub page: usize,
    /// Where it points — an address, or a page of this document.
    pub target: Target,
    /// The clickable rectangle in render coordinates (top-left origin), the
    /// same space every other box in this crate uses.
    pub bbox: BBox,
}

/// Cap on annotations read from one file. `/Annots` is an arbitrary array in
/// an arbitrary object graph, and a hostile file may make it enormous.
const MAX_LINKS: usize = 20_000;

/// How much of the annotation rectangle an element must cover to own the link.
///
/// Link rectangles are drawn generously — a viewer wants a target big enough
/// to hit — so an exact containment test loses most of them. Half is enough to
/// pick the right element out of a page while still rejecting a rectangle that
/// merely brushes a neighbour.
const MIN_COVERAGE: f64 = 0.5;

/// Every link annotation in the document whose target can be resolved, in
/// page order.
///
/// Both kinds are read. An external URI is the obvious content; an internal
/// jump matters just as much, because a table of contents is a page of links
/// and dropping them leaves a reader — human or model — looking at underlined
/// text with nothing behind it. A destination that resolves to no page of this
/// file is not reported: a target nobody can follow is not information.
pub fn extract(pdf: &Pdf) -> Vec<Link> {
    let pages = pdf.pages();
    // Page object → index, for resolving destinations.
    let index_of: HashMap<(i32, i32), usize> = pages
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let id = ObjRef::from(p.raw().obj_id()?);
            Some(((id.obj_number, id.gen_number), i))
        })
        .collect();
    let mut out = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        let Some(annots) = page.raw().get::<Array<'_>>(b"Annots") else {
            continue;
        };
        // Page height for the top-left origin flip.
        let (_, height) = page.render_dimensions();
        for a in annots.iter::<Dict<'_>>() {
            if out.len() >= MAX_LINKS {
                return out;
            }
            let is_link = a
                .get::<Name<'_>>(b"Subtype")
                .is_some_and(|n| n.as_ref() == b"Link");
            if !is_link {
                continue;
            }
            let (Some(target), Some(bbox)) = (target_of(&a, &index_of), rect_of(&a, height as f64))
            else {
                continue;
            };
            // A jump that lands on the page it starts from takes a reader
            // nowhere they are not: a figure cross-reference beside its own
            // figure. It is a link in the file and no navigation in the
            // document, so it is not reported as one.
            if target == (Target::Page { page: page_index }) {
                continue;
            }
            out.push(Link {
                page: page_index,
                target,
                bbox,
            });
        }
    }
    out
}

/// Where a link annotation points: its action's URI, or the page its
/// destination lands on.
fn target_of(annot: &Dict<'_>, index_of: &HashMap<(i32, i32), usize>) -> Option<Target> {
    if let Some(action) = annot.get::<Dict<'_>>(b"A") {
        match action.get::<Name<'_>>(b"S").as_ref().map(|n| n.as_ref()) {
            Some(b"URI") => {
                let uri = decode_uri(action.get::<PdfString>(b"URI")?.as_bytes());
                return (!uri.is_empty()).then_some(Target::Uri { uri });
            }
            // A jump expressed as an action rather than as `/Dest`; the
            // destination array has the same shape in both places.
            Some(b"GoTo") => {
                return dest_page(&action.get::<Array<'_>>(b"D")?, index_of)
                    .map(|page| Target::Page { page })
            }
            // `/GoToR`, `/Launch`, `/JavaScript` and the rest address another
            // file or an action, not a place in this document.
            _ => return None,
        }
    }
    // A named destination (`/Dest /chapter1`) resolves through the catalog's
    // name tree, which this does not walk: the array form covers what
    // generators actually emit.
    dest_page(&annot.get::<Array<'_>>(b"Dest")?, index_of).map(|page| Target::Page { page })
}

/// The page index a destination array lands on. Its first element is the page
/// object; everything after it is the view to scroll to, which has no meaning
/// for an element stream.
fn dest_page(dest: &Array<'_>, index_of: &HashMap<(i32, i32), usize>) -> Option<usize> {
    let r = dest.raw_iter().next()?.as_obj_ref()?;
    index_of.get(&(r.obj_number, r.gen_number)).copied()
}

/// Decode a URI string. The spec says a URI is 7-bit ASCII with anything else
/// percent-encoded, but files carry UTF-16BE here anyway, so the BOM decides —
/// the same rule the outline and form readers apply to text strings.
fn decode_uri(bytes: &[u8]) -> String {
    let s = if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    };
    // A URI cannot contain literal whitespace, and trailing NULs are common in
    // strings padded by generators.
    s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .to_string()
}

/// The annotation rectangle, flipped to a top-left origin.
///
/// `/Rect` is any two opposite corners, in either order, so both axes are
/// normalized rather than assumed.
fn rect_of(annot: &Dict<'_>, page_height: f64) -> Option<BBox> {
    let r = annot.get::<Array<'_>>(b"Rect")?;
    let v: Vec<f64> = r.iter::<Number>().map(|n| n.as_f64()).collect();
    if v.len() != 4 {
        return None;
    }
    let b = BBox {
        x0: v[0].min(v[2]),
        y0: page_height - v[1].max(v[3]),
        x1: v[0].max(v[2]),
        y1: page_height - v[1].min(v[3]),
    };
    (b.width() > 0.0 && b.height() > 0.0).then_some(b)
}

/// Attach each link to the element it covers, locating the anchor inside that
/// element's text where possible.
///
/// Called after the model tiers, not before: an escalated reading replaces the
/// text a link's anchor has to be found in, and resolving the anchor against
/// text that is about to be discarded produces offsets into a string nobody
/// ever sees.
pub fn attach(elements: &mut [Element], links: &[Link], pages: &[Page]) {
    if links.is_empty() {
        return;
    }
    // Reading order within an element, so several links in one paragraph are
    // resolved left to right and each search can start where the last ended.
    let mut ordered: Vec<&Link> = links.iter().collect();
    ordered.sort_by(|a, b| {
        (a.page, a.bbox.y0.round() as i64, a.bbox.x0.round() as i64)
            .partial_cmp(&(b.page, b.bbox.y0.round() as i64, b.bbox.x0.round() as i64))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for link in ordered {
        let Some(owner) = owner_of(elements, link) else {
            continue;
        };
        let anchor = pages
            .get(link.page)
            .map(|p| anchor_text(p, link.bbox))
            .unwrap_or_default();
        // Resume after the last anchor already placed, so a page of identical
        // link texts ("here", "here", "here") does not resolve them all onto
        // the first occurrence.
        let from = elements[owner]
            .links
            .as_ref()
            .and_then(|ls| ls.last().and_then(|l| l.end))
            .unwrap_or(0);
        let new = ElementLink {
            target: link.target.clone(),
            begin: None,
            end: None,
        };
        let new = match locate(&elements[owner].text, &anchor, from) {
            Some((b, e)) => new.spanning(b, e),
            None => new,
        };
        let slot = elements[owner].links.get_or_insert_with(Vec::new);
        // The same target annotated twice over one anchor — a link split
        // across two rectangles for a wrapped line — is one link.
        if !slot.contains(&new) {
            slot.push(new);
        }
    }

    for e in elements.iter_mut() {
        let text: Vec<char> = e.text.chars().collect();
        if let Some(ls) = e.links.as_mut() {
            ls.sort_by_key(|l| l.begin.unwrap_or(usize::MAX));
            // Overlapping anchors cannot both be spliced into one string, and
            // the shorter one is the one whose rectangle was less specific.
            let mut last_end = 0usize;
            ls.retain(|l| match l.span() {
                Some((b, end)) if b >= last_end => {
                    last_end = end;
                    true
                }
                Some(_) => false,
                None => true,
            });
            merge_wrapped(ls, &text);
        }
    }
}

/// Join anchors that a line break split.
///
/// A link over wrapped text is annotated once per line, so one address arrives
/// as two rectangles and becomes two anchors with a space between them — a URL
/// broken mid-path, marked up twice. Where the same target's anchors are
/// separated by nothing but whitespace they are one link, and joining them is
/// what makes the emitted anchor read as the thing the page underlined.
fn merge_wrapped(links: &mut Vec<ElementLink>, text: &[char]) {
    let mut i = 0;
    while i + 1 < links.len() {
        let joinable = match (links[i].span(), links[i + 1].span()) {
            (Some((_, a_end)), Some((b_start, _))) => {
                links[i].target == links[i + 1].target
                    && b_start >= a_end
                    && text[a_end..b_start].iter().all(|c| c.is_whitespace())
            }
            _ => false,
        };
        if joinable {
            links[i].end = links[i + 1].end;
            links.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// The element covering most of a link's rectangle.
fn owner_of(elements: &[Element], link: &Link) -> Option<usize> {
    elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.page == link.page && !e.text.is_empty())
        .filter_map(|(i, e)| {
            let b = e.bbox?;
            let w = (b.x1.min(link.bbox.x1) - b.x0.max(link.bbox.x0)).max(0.0);
            let h = (b.y1.min(link.bbox.y1) - b.y0.max(link.bbox.y0)).max(0.0);
            let area = (link.bbox.width() * link.bbox.height()).max(1e-9);
            let coverage = w * h / area;
            (coverage >= MIN_COVERAGE).then_some((i, coverage))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

/// The text a link's rectangle sits over, read from the page's own glyphs.
///
/// Line assembly rather than raw concatenation, because the space between two
/// words is an advance, not a glyph: joining glyph strings directly yields
/// `AbouthePublisher`, which matches nothing.
///
/// Public because a deep reader has to be told which words carry which
/// address, and it is looking at pixels: the anchor is the only handle it has
/// on a link it cannot otherwise see.
pub fn anchor_text(page: &Page, rect: BBox) -> String {
    let inside: Vec<crate::glyph::Glyph> = page
        .glyphs
        .iter()
        .filter(|g| {
            let (x, y) = g.center();
            rect.contains(x, y)
        })
        .cloned()
        .collect();
    if inside.is_empty() {
        return String::new();
    }
    crate::line::assemble(&inside)
        .iter()
        .map(|l| l.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find `anchor` in `text` at or after char offset `from`, returning its char
/// range.
///
/// Whitespace-insensitive on both sides. The anchor is assembled from a
/// handful of glyphs inside one rectangle while the element's text came from
/// the whole page, and the space between two words is inferred from the
/// advances around it — a subset can infer a different one. Matching on the
/// visible characters and mapping back to the full string's offsets sidesteps
/// the disagreement entirely.
fn locate(text: &str, anchor: &str, from: usize) -> Option<(usize, usize)> {
    let squeeze = |s: &str, skip: usize| -> (String, Vec<usize>) {
        let mut out = String::new();
        let mut map = Vec::new();
        for (i, c) in s.chars().enumerate().skip(skip) {
            if !c.is_whitespace() {
                out.push(c);
                map.push(i);
            }
        }
        (out, map)
    };
    let (hay, map) = squeeze(text, from);
    let (needle, _) = squeeze(anchor, 0);
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let at = hay.find(&needle)?;
    let start = hay[..at].chars().count();
    let len = needle.chars().count();
    Some((map[start], map[start + len - 1] + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(text: &str, bbox: BBox) -> Element {
        Element {
            id: String::new(),
            kind: "doco:Paragraph".into(),
            page: 0,
            bbox: Some(bbox),
            text: text.into(),
            level: None,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "rust",
            evidence: "layout",
        }
    }

    fn bbox(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    fn uri(u: &str) -> Target {
        Target::Uri { uri: u.into() }
    }

    #[test]
    fn locates_an_anchor_inside_a_sentence() {
        assert_eq!(
            locate("see the filing for detail", "the filing", 0),
            Some((4, 14))
        );
    }

    #[test]
    fn spacing_disagreement_does_not_prevent_a_match() {
        // The rectangle's glyphs assembled with a space the page did not use.
        assert_eq!(
            locate("About the Publisher", "About thePublisher", 0),
            Some((0, 19))
        );
    }

    #[test]
    fn a_repeated_anchor_resolves_to_the_next_occurrence() {
        let text = "here and here";
        let first = locate(text, "here", 0).unwrap();
        assert_eq!(first, (0, 4));
        assert_eq!(locate(text, "here", first.1), Some((9, 13)));
    }

    #[test]
    fn an_anchor_that_is_not_in_the_text_has_no_span() {
        assert_eq!(locate("About the Publisher", "Index", 0), None);
    }

    #[test]
    fn a_link_lands_on_the_element_it_covers() {
        let mut elements = vec![
            el("About the Publisher", bbox(10.0, 10.0, 200.0, 24.0)),
            el("Index", bbox(10.0, 40.0, 200.0, 54.0)),
        ];
        let links = vec![Link {
            page: 0,
            target: uri("https://example.org/pub"),
            bbox: bbox(12.0, 11.0, 120.0, 23.0),
        }];
        attach(&mut elements, &links, &[]);
        // No page glyphs here, so the anchor cannot be located — the link
        // still belongs to the element it covers.
        assert_eq!(
            elements[0].links.as_deref(),
            Some(&[ElementLink::uri("https://example.org/pub")][..])
        );
        assert!(elements[1].links.is_none());
    }

    #[test]
    fn a_rectangle_over_nothing_is_dropped() {
        let mut elements = vec![el("About the Publisher", bbox(10.0, 10.0, 200.0, 24.0))];
        let links = vec![Link {
            page: 0,
            target: uri("https://example.org/pub"),
            bbox: bbox(300.0, 300.0, 400.0, 320.0),
        }];
        attach(&mut elements, &links, &[]);
        assert!(elements[0].links.is_none());
    }

    #[test]
    fn one_target_annotated_twice_is_one_link() {
        let mut elements = vec![el("About the Publisher", bbox(10.0, 10.0, 200.0, 24.0))];
        let rect = bbox(12.0, 11.0, 120.0, 23.0);
        let links = vec![
            Link {
                page: 0,
                target: uri("https://example.org/pub"),
                bbox: rect,
            },
            Link {
                page: 0,
                target: uri("https://example.org/pub"),
                bbox: rect,
            },
        ];
        attach(&mut elements, &links, &[]);
        assert_eq!(elements[0].links.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn an_internal_jump_renders_as_a_one_based_page_fragment() {
        assert_eq!(ElementLink::page(3).href(), "#page=4");
    }
}
