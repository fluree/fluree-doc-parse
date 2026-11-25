//! Fluree PDF extraction core.
//!
//! Two layers. The extraction layer parses a PDF into positioned glyphs,
//! normalizes the text while preserving the offset→bbox mapping, and resolves
//! entity spans back to rectangles for overlay rendering ([`extract_file`],
//! [`Glyph`], [`text`], [`overlay`]).
//!
//! Above it, the layout layer infers structure from geometry: glyphs into
//! [lines](mod@line) and [`block`]s, [columns](mod@column) and reading order, [`heading`] levels
//! cross-checked against the PDF [`outline`] tree, [`rule`]-driven and
//! clustered [`table`] grids, page [`furniture`] stripped before assembly, and
//! [`doco`] emission on top. [`route`] and [`arbiter`] decide, per region,
//! where deterministic inference is too weak to trust and a model tier should
//! arbitrate the structure.
//!
//! # What is a compatibility surface
//!
//! Two tiers, and the distinction is load-bearing because most of this crate
//! is `pub` for a mechanical reason rather than a deliberate one: `fdoc` is a
//! separate crate, so everything `fdoc dev` inspects has to be reachable.
//!
//! **Supported.** These follow semver and are what a consumer should build on:
//!
//! - [`extract_file`] / [`extract_bytes`] → [`Document`]
//! - [`image::page_of`] — a bare raster image as a one-page document
//! - [`document::analyze_with`] → `Analysis`, plus [`document::to_markdown`]
//!   and [`document::to_xhtml`]
//! - [`doco::to_doco`] / [`doco::to_text`] and [`doco::DocoOptions`]
//! - [`route::decide`] and [`route::signals`] — escalation verdicts
//! - [`unread_pages`] and [`unread_regions`] — what the router sent a reader
//!   to and nothing read
//! - [`parse_warnings`] — what the PDF interpreter reported, per page
//! - [`forms::fields`] — AcroForm values
//! - [`outline::extract`] — the PDF bookmark tree
//! - [`link::extract`] / [`link::attach`] — link annotations and their anchors
//! - [`arbiter::TierBackend`] and [`arbiter::splice`] — the model-tier contract
//! - [`ocr::inject`] and [`ocr::mark_provenance`] — OCR words as glyphs
//! - [`overlay::rects_for_glyph_range`] — glyph span → highlight rectangles
//! - [`overlay::highlight`] — a text-projection span → its page and rectangles
//! - `render::page` — rasterising a page, behind the `render` feature
//!
//! The element model itself lives in `fluree-doc-model`, which is the crate to
//! depend on if you only consume elements and never parse a PDF.
//!
//! **Internal.** Everything else — [`block`], [columns](mod@column), [`dedup`],
//! [`furniture`], [`geom`], [`heading`], [lines](mod@line), [`rule`],
//! [`table`], [`text`] — is layout machinery whose shape follows whatever the
//! measurements demand. It is documented because the reasoning is worth
//! reading, not because it is stable, and it may change in any release. This
//! is the same promise `fdoc dev` makes about its output.

pub mod arbiter;
pub mod block;
pub(crate) mod cells;
pub mod column;
pub mod dedup;
pub mod doco;
pub mod document;
#[cfg(feature = "render")]
pub mod escalate;
pub(crate) mod extract;
pub(crate) mod faces;
pub mod fidelity;
pub mod figure;
pub mod forms;
pub mod furniture;
pub mod geom;
pub(crate) mod glyph;
pub mod heading;
pub mod image;
pub mod info;
pub(crate) mod ligature;
pub mod line;
pub mod link;
pub(crate) mod marked;
pub mod ocr;
pub mod outline;
pub mod overlay;
pub(crate) mod region;

/// The [`unread_pages`] reason of a page an OCR reading read whole that
/// still holds next to no text.
pub const OCR_SPARSE: &str = "OcrSparse";

/// Pages the router says carry content, which the final element stream does
/// not hold.
///
/// Called after the tiers, because "unread" is only true once whatever was
/// going to read it has run. The router's verdict alone is not the answer: a
/// page that escalated and came back is read, and a page nobody escalated is
/// not.
///
/// Routing is not consulted for every page — it is not free, and a page with
/// a normal amount of text cannot be one of these. Only pages the output is
/// nearly empty for are asked about.
pub fn unread_pages(
    doc: &Document,
    elements: &[fluree_doc_model::Element],
) -> Vec<fluree_doc_model::UnreadPage> {
    /// Characters a page's elements must fall below to be worth asking the
    /// router about. A page holding real prose is not an unread page whatever
    /// the router would say.
    const NEARLY_EMPTY: usize = 40;
    let mut out = Vec::new();
    for p in &doc.pages {
        // A running header kept once says nothing about whether the page
        // under it was read.
        let text: usize = elements
            .iter()
            .filter(|e| e.page == p.index && !matches!(e.evidence, "header" | "footer"))
            .map(|e| e.text.trim().chars().count())
            .sum();
        // A page a model's page reading read whole was read, however little
        // it held: the model is the last reader there is.
        let page_read = elements
            .iter()
            .any(|e| e.page == p.index && e.evidence == "page-tier");
        if text >= NEARLY_EMPTY || page_read {
            continue;
        }
        // A page an OCR reading read whole was read, and asks no model for
        // more; but an engine that gave a few words for a dense scan it could
        // not read leaves the page as unread as before. Said, so it is not
        // taken for a page with nothing on it.
        if p.ocr.as_ref().is_some_and(|o| o.whole) {
            out.push(fluree_doc_model::UnreadPage {
                index: p.index,
                reason: OCR_SPARSE.into(),
            });
            continue;
        }
        if let route::Route::Vlm(reason) = p.routing().route {
            out.push(fluree_doc_model::UnreadPage {
                index: p.index,
                reason: format!("{reason:?}"),
            });
        }
    }
    out
}

/// Raster regions nothing read: those the router sends a reader to, then
/// those it declines.
///
/// The region-level counterpart of [`unread_pages`], for a page whose text
/// layer is fine but which carries a screenshot or a scanned table the text
/// layer does not cover ([`route::Route::VlmRegions`]). Such a page holds
/// plenty of text, so it is never an unread page, and without this the
/// region's content would be missing with nothing saying so.
///
/// Called after the tiers, like [`unread_pages`]. A region counts as read
/// when an OCR reading was taken for it (or for its whole page), when a
/// model's reading of it was spliced in (an element `region-splice` or
/// `region-chart` in its box), or when a model's reading of the whole page
/// replaced the page. A reading that came back empty, or a whole-page one
/// the splice refused as truncated, read nothing.
///
/// Listed with the reason `Region`. After them, on any page not read whole,
/// the regions the router declines to send a model to
/// ([`route::declined_regions`]), so a caller can decide for itself:
/// `Small`, texty but under a tenth of the page (and not a checkbox or an
/// icon), which an OCR engine reads and is not listed once it has; and
/// `NotTexty`, a tenth of the page or more whose pixels the probe does not
/// take for text, which nothing reads and is always listed. Only `Region`
/// is worth a warning: the declined ones are logos, photographs and charts
/// as often as content.
pub fn unread_regions(
    doc: &Document,
    elements: &[fluree_doc_model::Element],
) -> Vec<fluree_doc_model::UnreadRegion> {
    let mut out = Vec::new();
    for p in &doc.pages {
        // An OCR reading of the whole page read all of it: its glyphs can
        // turn the page Deterministic, and the image under them would then
        // look declined.
        if p.ocr.as_ref().is_some_and(|o| o.whole) {
            continue;
        }
        // As routed before analysis cleaned the page: the numbers an OCR
        // reading, a crop and an anchor name a region by.
        let routing = p.routing();
        let regions: &[BBox] = match &routing.route {
            route::Route::Vlm(_) => continue,
            route::Route::VlmRegions(r) => r,
            route::Route::Deterministic => &[],
        };
        let on_page = || elements.iter().filter(|e| e.page == p.index);
        // A model's reading of the whole page holds its regions too.
        if on_page().any(|e| e.evidence == "page-tier") {
            continue;
        }
        // The boxes listed on this page: an image the page draws twice in
        // one place is one region to read, and one to report.
        let mut listed = std::collections::HashSet::new();
        for (i, b) in regions.iter().enumerate() {
            if p.ocr_read_region(i) {
                continue;
            }
            // Spliced in at the anchor's place, the reading keeps the box
            // the anchor was given: the region's, as displayed.
            let shown = p.to_display(*b);
            if listed.contains(&box_key(&shown)) {
                continue;
            }
            let spliced = on_page().any(|e| {
                matches!(e.evidence, "region-splice" | "region-chart")
                    && e.bbox.is_some_and(|eb| same_box(&eb, &shown))
            });
            if !spliced {
                listed.insert(box_key(&shown));
                out.push(fluree_doc_model::UnreadRegion {
                    page: p.index,
                    bbox: shown,
                    reason: "Region".into(),
                });
            }
        }
        // A small region is numbered among the regions an OCR engine reads
        // after the routed ones; that number says whether it was read. One
        // the probe does not take for text has none: nothing reads it.
        for &(b, why, ocr_number) in &routing.declined {
            if ocr_number.is_some_and(|i| p.ocr_read_region(i)) {
                continue;
            }
            let shown = p.to_display(b);
            if listed.insert(box_key(&shown)) {
                out.push(fluree_doc_model::UnreadRegion {
                    page: p.index,
                    bbox: shown,
                    reason: format!("{why:?}"),
                });
            }
        }
    }
    out
}

/// What the PDF interpreter reported on each page ([`Page::warnings`]),
/// once per page and kind, with how many times, in page order.
pub fn parse_warnings(doc: &Document) -> Vec<fluree_doc_model::ParseWarning> {
    let mut out: Vec<fluree_doc_model::ParseWarning> = Vec::new();
    for p in &doc.pages {
        for &kind in &p.warnings {
            match out.iter_mut().find(|w| w.page == p.index && w.kind == kind) {
                Some(w) => w.count += 1,
                None => out.push(fluree_doc_model::ParseWarning {
                    page: p.index,
                    kind: kind.into(),
                    count: 1,
                }),
            }
        }
    }
    out
}

/// Two boxes for the same region, computed the same way: equal but for
/// floating-point noise.
fn same_box(a: &BBox, b: &BBox) -> bool {
    const EPS: f64 = 0.01;
    (a.x0 - b.x0).abs() < EPS
        && (a.y0 - b.y0).abs() < EPS
        && (a.x1 - b.x1).abs() < EPS
        && (a.y1 - b.y1).abs() < EPS
}

/// A box as a set key, to the hundredth of a unit [`same_box`] tolerates:
/// the same image drawn twice in one place gives the same box, computed the
/// same way, and the set keeps listing it from walking the list per region.
fn box_key(b: &BBox) -> [i64; 4] {
    [b.x0, b.y0, b.x1, b.y1].map(|v| (v * 100.0).round() as i64)
}

#[cfg(feature = "render")]
pub mod render;
pub mod route;
pub mod rule;
pub mod table;
pub mod text;
pub mod textart;
pub mod textgrid;
pub mod watermark;

pub use block::Block;
pub use extract::{extract_bytes, extract_file, Document, ExtractError, Page};
pub use furniture::Furniture;
pub use geom::BBox;
pub use glyph::Glyph;
pub use line::Line;
pub use text::PageText;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{ImagePlacement, OcrRead};
    use fluree_doc_model::Element;

    fn b(x0: f64, y0: f64, x1: f64, y1: f64) -> BBox {
        BBox { x0, y0, x1, y1 }
    }

    /// A page of prose in its top band (1,500 glyphs between y 10 and 20),
    /// with these texty images and no glyph over them.
    fn page(images: &[BBox]) -> Page {
        let glyphs = (0..1500)
            .map(|i| Glyph {
                text: "a".into(),
                bbox: Some(b(10.0, 10.0, 15.0, 20.0)),
                page: 0,
                origin: (10.0, 20.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                weight: None,
                advance: None,
                draw_index: i,
            })
            .collect();
        Page {
            index: 0,
            glyphs,
            images: images
                .iter()
                .map(|&bbox| ImagePlacement {
                    bbox,
                    texty: true,
                    ppi: None,
                    bitonal: false,
                    grid: (1000, 1000),
                    key: None,
                })
                .collect(),
            rules: Vec::new(),
            routing: None,
            dots: Vec::new(),
            fills: Vec::new(),
            width: 600.0,
            height: 800.0,
            rotation: 0,
            ocr: None,
            warnings: Vec::new(),
        }
    }

    const BIG: BBox = BBox {
        x0: 50.0,
        y0: 300.0,
        x1: 550.0,
        y1: 500.0,
    };
    const OTHER: BBox = BBox {
        x0: 50.0,
        y0: 550.0,
        x1: 550.0,
        y1: 750.0,
    };

    /// A model's reading spliced in where the splice puts one.
    fn spliced(bbox: BBox, evidence: &'static str) -> Element {
        Element {
            bbox: Some(bbox),
            ..Element::new("doco:Paragraph", "Account 4417", "vlm", evidence)
        }
    }

    #[test]
    fn a_routed_region_nothing_read_is_unread_with_its_box() {
        let doc = Document {
            pages: vec![page(&[BIG, OTHER])],
        };
        let got = unread_regions(&doc, &[]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].page, got[0].bbox), (0, BIG));
        assert_eq!(got[0].reason, "Region");
        assert_eq!(got[1].bbox, OTHER);
        // A page of prose is never an unread page: the region is the trace.
        assert!(unread_pages(&doc, &[]).is_empty());
    }

    #[test]
    fn a_region_a_model_read_is_not_unread() {
        let doc = Document {
            pages: vec![page(&[BIG, OTHER])],
        };
        for evidence in ["region-splice", "region-chart"] {
            let got = unread_regions(&doc, &[spliced(BIG, evidence)]);
            assert_eq!(got.len(), 1, "{evidence}: {got:?}");
            assert_eq!(got[0].bbox, OTHER);
        }
        // An anchor left in place (`--emit-anchors`, no reading) is not one.
        let anchor = Element {
            bbox: Some(BIG),
            ..Element::new("doco:Figure", "[[VLM:p0:r0]]", "rust", "route")
        };
        assert_eq!(unread_regions(&doc, &[anchor]).len(), 2);
        // A reading elsewhere on the page does not read this region.
        let elsewhere = spliced(b(50.0, 100.0, 550.0, 200.0), "region-splice");
        assert_eq!(unread_regions(&doc, &[elsewhere]).len(), 2);
        // Nor does one on another page.
        let mut other_page = spliced(BIG, "region-splice");
        other_page.page = 1;
        assert_eq!(unread_regions(&doc, &[other_page]).len(), 2);
    }

    #[test]
    fn an_image_drawn_twice_in_one_place_is_one_unread_region() {
        let doc = Document {
            pages: vec![page(&[BIG, OTHER, BIG])],
        };
        let got: Vec<BBox> = unread_regions(&doc, &[]).iter().map(|u| u.bbox).collect();
        assert_eq!(got, [BIG, OTHER]);
        // The same box on another page is another region.
        let mut next = page(&[BIG]);
        next.index = 1;
        let doc = Document {
            pages: vec![page(&[BIG]), next],
        };
        assert_eq!(unread_regions(&doc, &[]).len(), 2);
    }

    #[test]
    fn a_whole_page_reading_reads_every_region() {
        let doc = Document {
            pages: vec![page(&[BIG, OTHER])],
        };
        let whole = Element::new("doco:Paragraph", "the page", "vlm", "page-tier");
        assert!(unread_regions(&doc, &[whole]).is_empty());
    }

    #[test]
    fn a_region_ocr_read_is_not_unread() {
        let mut p = page(&[BIG, OTHER]);
        p.ocr = Some(OcrRead {
            first_glyph: 1500,
            whole: false,
            regions: vec![1],
        });
        let doc = Document {
            pages: vec![p.clone()],
        };
        let got = unread_regions(&doc, &[]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].bbox, BIG);
        // A whole-page OCR reading read them all.
        p.ocr.as_mut().unwrap().whole = true;
        let doc = Document { pages: vec![p] };
        assert!(unread_regions(&doc, &[]).is_empty());
    }

    /// `(reason, bbox)` of each unread region.
    fn reasons(doc: &Document) -> Vec<(String, BBox)> {
        unread_regions(doc, &[])
            .into_iter()
            .map(|u| (u.reason, u.bbox))
            .collect()
    }

    #[test]
    fn a_region_too_small_for_a_model_is_listed_as_small_until_ocr_reads_it() {
        // A logo-sized capture: an OCR engine is offered it, a model not.
        // Listed, for a caller to judge, after the routed regions.
        let small = b(50.0, 100.0, 250.0, 130.0);
        let doc = Document {
            pages: vec![page(&[small, BIG])],
        };
        assert_eq!(route::ocr_regions(&doc.pages[0]), vec![BIG, small]);
        assert_eq!(
            reasons(&doc),
            [("Region".into(), BIG), ("Small".into(), small)]
        );
        // OCR read it: region 1 among those an engine reads.
        let mut p = page(&[small, BIG]);
        p.ocr = Some(OcrRead {
            first_glyph: 1500,
            whole: false,
            regions: vec![1],
        });
        let doc = Document { pages: vec![p] };
        assert_eq!(reasons(&doc), [("Region".to_string(), BIG)]);
        // Alone on a page of prose, it is listed too: the page routes
        // nothing, and the region is still the router's to decline.
        let doc = Document {
            pages: vec![page(&[small])],
        };
        assert_eq!(reasons(&doc), [("Small".to_string(), small)]);
    }

    #[test]
    fn a_large_image_the_probe_does_not_take_for_text_is_listed_as_not_texty() {
        let mut p = page(&[BIG, b(50.0, 100.0, 70.0, 120.0)]);
        // A photograph a fifth of the page, and an icon: only the first is
        // worth a caller's look.
        for i in &mut p.images {
            i.texty = false;
        }
        let doc = Document {
            pages: vec![p.clone()],
        };
        assert_eq!(route::decide(&doc.pages[0]).0, route::Route::Deterministic);
        assert_eq!(reasons(&doc), [("NotTexty".to_string(), BIG)]);
        // No OCR engine is offered it, so no reading of a region can clear
        // it: it stays listed.
        assert!(route::ocr_regions(&doc.pages[0]).is_empty());
        let mut read = p.clone();
        read.ocr = Some(OcrRead {
            first_glyph: 1500,
            whole: false,
            regions: vec![0],
        });
        let doc = Document { pages: vec![read] };
        assert_eq!(reasons(&doc), [("NotTexty".to_string(), BIG)]);
        // Under text the page's own glyphs cover, it is not listed: the text
        // layer reads it (a background behind real text).
        let mut covered = p;
        covered.glyphs.extend((0..40).map(|i| Glyph {
            text: "b".into(),
            bbox: Some(b(100.0, 350.0, 105.0, 360.0)),
            page: 0,
            origin: (100.0, 360.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            advance: None,
            draw_index: 1500 + i,
        }));
        let doc = Document {
            pages: vec![covered],
        };
        assert!(reasons(&doc).is_empty());
        // A model's reading of the whole page reads it too.
        let doc = Document {
            pages: vec![page(&[BIG])],
        };
        let mut photo = doc.clone();
        photo.pages[0].images[0].texty = false;
        let whole = Element::new("doco:Paragraph", "the page", "vlm", "page-tier");
        assert!(unread_regions(&photo, &[whole]).is_empty());
    }

    #[test]
    fn a_declined_image_drawn_twice_in_one_place_is_listed_once() {
        let small = b(50.0, 100.0, 250.0, 130.0);
        let doc = Document {
            pages: vec![page(&[small, BIG, small])],
        };
        assert_eq!(
            reasons(&doc),
            [("Region".into(), BIG), ("Small".into(), small)]
        );
        let mut p = page(&[BIG, BIG]);
        for i in &mut p.images {
            i.texty = false;
        }
        let doc = Document { pages: vec![p] };
        assert_eq!(reasons(&doc), [("NotTexty".to_string(), BIG)]);
    }

    #[test]
    fn a_scan_ocr_read_whole_lists_no_region() {
        // A scan the probe does not take for text, read whole by OCR: the
        // OCR glyphs route the page Deterministic, and its image is not a
        // declined region for that.
        let mut p = page(&[b(0.0, 0.0, 600.0, 800.0)]);
        p.images[0].texty = false;
        for (i, g) in p.glyphs.iter_mut().take(150).enumerate() {
            g.bbox = Some(b(
                50.0,
                100.0 + i as f64 * 4.0,
                55.0,
                103.0 + i as f64 * 4.0,
            ));
        }
        p.glyphs.truncate(150);
        p.ocr = Some(OcrRead {
            first_glyph: 0,
            whole: true,
            regions: Vec::new(),
        });
        assert_eq!(route::decide(&p).0, route::Route::Deterministic);
        let doc = Document { pages: vec![p] };
        assert!(unread_regions(&doc, &[]).is_empty());
        // Handed no element, the reading left the page under the floor: it
        // is listed as one OCR read sparsely, not as an unread scan.
        let pages = unread_pages(&doc, &[]);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].reason, OCR_SPARSE);
    }

    #[test]
    fn a_turned_pages_region_is_reported_as_displayed() {
        // Read in a turned frame, the region's box is turned back to where
        // the page shows it, the frame every element's box is in.
        let mut p = page(&[BIG]);
        p.rotation = 90;
        let shown = p.to_display(BIG);
        assert_ne!(shown, BIG);
        let doc = Document { pages: vec![p] };
        let got = unread_regions(&doc, &[]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].bbox, shown);
        // And a reading spliced at the displayed box reads it.
        assert!(unread_regions(&doc, &[spliced(shown, "region-splice")]).is_empty());
    }

    #[test]
    fn parser_warnings_are_listed_once_a_page_and_kind() {
        let mut a = page(&[]);
        a.warnings = vec![
            "ImageDecodeFailure",
            "UnsupportedFont",
            "ImageDecodeFailure",
        ];
        let mut b = page(&[]);
        b.index = 1;
        b.warnings = vec!["ImageDecodeFailure"];
        let doc = Document {
            pages: vec![a, page(&[]), b],
        };
        let got = parse_warnings(&doc);
        let got: Vec<(usize, &str, usize)> = got
            .iter()
            .map(|w| (w.page, w.kind.as_str(), w.count))
            .collect();
        assert_eq!(
            got,
            [
                (0, "ImageDecodeFailure", 2),
                (0, "UnsupportedFont", 1),
                (1, "ImageDecodeFailure", 1)
            ]
        );
    }

    #[test]
    fn a_page_read_whole_by_the_router_has_no_regions() {
        // A scan is an unread page, not a page of unread regions.
        let mut p = page(&[b(0.0, 0.0, 600.0, 800.0)]);
        p.glyphs.clear();
        let doc = Document { pages: vec![p] };
        assert!(unread_regions(&doc, &[]).is_empty());
        assert_eq!(unread_pages(&doc, &[]).len(), 1);
        // A model's reading of the page read it, however short.
        let whole = Element::new("doco:Paragraph", "Short note", "vlm", "page-tier");
        assert!(unread_pages(&doc, &[whole]).is_empty());
        // Short text of another kind does not.
        let layout = Element::new("doco:Paragraph", "Short note", "rust", "layout");
        assert_eq!(unread_pages(&doc, &[layout]).len(), 1);
    }
}
