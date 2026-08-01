//! The document element model, shared by every source format.

use crate::geom::BBox;

/// Where a link points.
///
/// The two kinds are kept apart rather than collapsed into one string, because
/// they are different facts: one addresses the world and one addresses this
/// document. A consumer loading a graph wants to follow the first and resolve
/// the second, and a `#page=4` standing in for both would make the internal
/// jump look like an address it is not.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum Target {
    /// An address outside the document, as the source states it. Not resolved
    /// and not validated: a relative target stays relative and a broken one
    /// stays broken, because the document said it.
    Uri { uri: String },
    /// A place inside the document — the 0-based index of the page the jump
    /// lands on, the same space as [`Element::page`].
    Page { page: usize },
}

/// A hyperlink covering some of an element's text.
///
/// A link is not drawn. In a PDF it is a rectangle and a target sitting beside
/// the content stream; in HTML and Markdown it is markup around the words. The
/// glyphs — or the text — are all that survives a reader that ignores it, so
/// the anchor reads as ordinary prose and the target is gone. Both are content:
/// a citation that points somewhere is a different fact from one that does not.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Link {
    #[serde(flatten)]
    pub target: Target,
    /// Char offset into the element's `text` where the anchor begins.
    ///
    /// Absent together with `end` where the link covers something with no text
    /// of its own — an image, a whole table cell — or where the anchor could
    /// not be located in the text. The link still belongs to the element; only
    /// its extent within it is unknown, and an emitter that needs a range to
    /// mark up leaves it alone rather than guessing one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub begin: Option<usize>,
    /// Char offset one past the anchor's last character.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
}

impl Link {
    /// A link to an address outside the document.
    pub fn uri(uri: impl Into<String>) -> Self {
        Link {
            target: Target::Uri { uri: uri.into() },
            begin: None,
            end: None,
        }
    }

    /// A link to a place inside the document, by 0-based page index.
    pub fn page(page: usize) -> Self {
        Link {
            target: Target::Page { page },
            begin: None,
            end: None,
        }
    }

    /// The same link with its anchor located: `text[begin..end]` in chars.
    pub fn spanning(mut self, begin: usize, end: usize) -> Self {
        self.begin = Some(begin);
        self.end = Some(end);
        self
    }

    /// The anchor's char range, when it is known.
    pub fn span(&self) -> Option<(usize, usize)> {
        match (self.begin, self.end) {
            (Some(b), Some(e)) if e > b => Some((b, e)),
            _ => None,
        }
    }

    /// The target written as a URL an emitter can put in an `href`.
    ///
    /// An internal jump becomes `#page=N`, 1-based, the fragment convention
    /// PDF viewers already use — the only form Markdown and HTML have for
    /// "elsewhere in this document".
    pub fn href(&self) -> String {
        match &self.target {
            Target::Uri { uri } => uri.clone(),
            Target::Page { page } => format!("#page={}", page + 1),
        }
    }
}

/// A DoCO-typed document element — the model every source format produces
/// and every emitter consumes.
///
/// This is the seam that makes the emitters source-agnostic: a PDF's
/// geometric inference, a Markdown parse and a DOCX's declared structure all
/// converge here, so Markdown/XHTML/DoCO/text output comes free for each.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Element {
    pub id: String,
    /// DoCO class, e.g. `doco:Paragraph`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Page index, 0-based. Formats without pagination report 0.
    pub page: usize,
    /// Where the element sits on the page, when the source has geometry.
    /// `None` for formats that carry structure but no layout (Markdown,
    /// DOCX): a zeroed box would read as a real position to every consumer
    /// that trusts coordinates, and entity overlay is one of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    pub text: String,
    /// Heading depth, 1-6. Only present on `doco:SectionTitle`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<usize>,
    /// Cells in row-major order. Only present on `doco:Table`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<Vec<String>>>,
    /// Measured count of leading header rows for `doco:Table` with `cells`
    /// (see `Grid::header_rows`). `None` where undetected (model-provided
    /// tables); consumers should treat that as 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_rows: Option<usize>,
    /// Row indices, below the header block, that are one full-width cell
    /// labelling the rows beneath them — the banner bands that split a
    /// matrix into sections. Empty where there are none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_headers: Option<Vec<usize>>,
    /// Row-major, same shape as `cells`: this cell continues the one above
    /// it (a vertical merge). `cells` follows the rowspan convention — the
    /// value sits where the text was laid out and the other spanned rows
    /// are blank — so a consumer needing self-contained rows denormalises
    /// through these flags (`table::denormalize`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_down: Option<Vec<bool>>,
    /// Row-major, same shape as `cells`: this cell continues the one to its
    /// left — the column boundary is not drawn across this row, as where a
    /// nested table rules columns the outer rows do not have.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_left: Option<Vec<bool>>,
    /// Identifier of the figure this element belongs to, shared by every
    /// fragment of one chart.
    ///
    /// A donut prints `20.0% 34.5% Latin America North America`; read in
    /// sequence that attaches the first percentage to the first label and
    /// gets half the chart wrong. The fragments are marked rather than
    /// merged: the page's own order is real information and is left alone,
    /// while the shared id says these belong to one drawing and their
    /// sequence is not a reading of it. A consumer that needs the pairing
    /// has the figure's box and can look at the drawing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figure: Option<String>,
    /// Hyperlinks over this element's text, in the order their anchors appear.
    ///
    /// Sorted by `begin`, non-overlapping, so an emitter can splice them into
    /// the text in one pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Link>>,
    /// Which engine produced this element. Always `"rust"` here; the VLM tier
    /// emits the same shape with `"vlm"`.
    pub provenance: &'static str,
    /// Which signal produced the classification — the basis of the confidence
    /// the router consumes.
    pub evidence: &'static str,
}

impl Element {
    /// The element's box, or an empty one for sources without geometry.
    ///
    /// Convenience for geometric pipelines (PDF), where every element has a
    /// box by construction. Consumers deciding *whether* there is geometry
    /// must read [`Element::bbox`] directly — this collapses that distinction
    /// on purpose so layout code stays readable.
    pub fn rect(&self) -> BBox {
        self.bbox.unwrap_or_default()
    }
}

/// A page whose content nothing read.
///
/// The router can tell that a page carries content the text layer does not
/// hold — a scan, a vector drawing, glyphs whose Unicode cannot be trusted.
/// When no reader then supplies it, the honest output is *empty for that
/// page*, and a consumer cannot tell that apart from a page that was blank.
/// One report produced 126 bytes of XHTML for a whole document of drawings
/// with nothing in it saying so.
///
/// Carried beside the elements rather than as one of them. A marker element
/// would have to hold text to be visible, and inventing text puts characters
/// into the projection that every `nif:beginIndex` in the graph is counted
/// against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnreadPage {
    /// 0-based physical page, the same space as [`Element::page`].
    #[serde(rename = "pageIndex")]
    pub index: usize,
    /// What the router saw: `Scanned`, `NearBlank` or `BrokenText`.
    pub reason: String,
}

/// Facts about a document that are not elements of it.
///
/// Additive: the emitters take this where they can carry it, and their
/// existing signatures stay valid with an empty one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notes {
    pub unread: Vec<UnreadPage>,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.unread.is_empty()
    }

    /// One line a human or a model can act on, or `None` when nothing is
    /// wrong. Deliberately prose: it ends up in a comment, and a comment
    /// nobody understands is not a warning.
    pub fn summary(&self) -> Option<String> {
        if self.unread.is_empty() {
            return None;
        }
        let mut pages: Vec<String> = self
            .unread
            .iter()
            .map(|u| (u.index + 1).to_string())
            .collect();
        pages.sort_by_key(|p| p.parse::<usize>().unwrap_or(0));
        let mut reasons: Vec<&str> = self.unread.iter().map(|u| u.reason.as_str()).collect();
        reasons.sort_unstable();
        reasons.dedup();
        Some(format!(
            "fluree-doc-parse: page{} {} carr{} content no reader transcribed ({}). \
             This output is missing it.",
            if pages.len() == 1 { "" } else { "s" },
            pages.join(", "),
            if pages.len() == 1 { "ies" } else { "y" },
            reasons.join(", ")
        ))
    }
}
