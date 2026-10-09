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

/// A stretch of speech: who said it, and where it sits in the recording.
///
/// The speaker is also written into the element's text, as `"<speaker>: "`
/// before the words. The text projection is what an extractor reads and what
/// every character offset counts against, and a claim with its author
/// attached is a different fact from the bare words. So the label always
/// occupies the first `speaker.chars().count()` characters of `text`, and a
/// consumer linking it to a person record needs no other offsets.
///
/// Times are kept here and nowhere in the text: a timestamp in the prose is
/// read as a time the speaker mentioned.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Turn {
    /// The speaker as the source names them (`"Ada Park"`, `"Speaker 2"`),
    /// unresolved. Absent for captions that name no one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Milliseconds from the start of the recording to the first word.
    pub start_ms: u64,
    /// Milliseconds from the start of the recording to the end of the last.
    pub end_ms: u64,
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
    /// What each cell holds, typed, where the source declares it: same
    /// shape as `cells`, and `None` for a cell the source holds as text.
    ///
    /// A workbook stores a number and shows it through a format: `0.12345`
    /// shows as `12%`, a serial day count as a date. `cells` keeps what is
    /// shown, because that is what the author saw and what the text
    /// projection reads; this keeps what is stored, at full precision. Only
    /// a source that declares types fills it. A guessed type from text
    /// (`45584888` a quantity rather than an order number, `03/04/2026` in
    /// one country's order) would be a reading of the page, not a fact
    /// about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datums: Option<Vec<Vec<Option<Datum>>>>,
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
    /// Who said this and when, for elements read from a recording's
    /// transcript. Absent everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<Turn>,
    /// The header of the message this element opens, on the first element of
    /// each message in an email. The elements after it, up to the next that
    /// opens or resumes one, are that message's body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Box<crate::message::Message>>,
    /// The id of the element that opened an earlier message, on the first
    /// element where that message goes on after a quote nested in it.
    ///
    /// A reply can quote inside a quote, and the outer message does not end
    /// where the inner one starts: a sender's signature, or a footer their
    /// server added, comes after everything they quoted. Read as the body of
    /// whichever message came last, a signature with an address in it
    /// belongs to someone else. The elements from here, up to the next that
    /// opens or resumes a message, are that earlier message's body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumes: Option<String>,
    /// Part of a message's signature: the sign-off and the block under it
    /// (name, title, company, phone, address, legal footer), in an email.
    /// It is the sender's, so whatever it states, an address above all, is
    /// about the sender and their organisation rather than about the
    /// people the message discusses.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub signature: bool,
    /// Which engine produced this element. Always `"rust"` here; the VLM tier
    /// emits the same shape with `"vlm"`.
    pub provenance: &'static str,
    /// Which signal produced the classification — the basis of the confidence
    /// the router consumes.
    pub evidence: &'static str,
}

impl Element {
    /// An element of `kind` holding `text`, from the reader `provenance`
    /// names, classified by the signal `evidence` names — and nothing else:
    /// the first page, no box, no table, no links.
    ///
    /// A reader writes what it knows over this
    /// (`Element { page, level, ..Element::new(kind, text, "pptx", "pptx") }`),
    /// so a field the model gains is absent from every reader until one
    /// fills it, rather than a change to each.
    pub fn new(
        kind: impl Into<String>,
        text: impl Into<String>,
        provenance: &'static str,
        evidence: &'static str,
    ) -> Self {
        Element {
            id: String::new(),
            kind: kind.into(),
            page: 0,
            bbox: None,
            text: text.into(),
            level: None,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            datums: None,
            figure: None,
            links: None,
            turn: None,
            message: None,
            resumes: None,
            signature: false,
            provenance,
            evidence,
        }
    }

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

/// A value as its source stores it, typed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Datum {
    /// The XSD datatype, as a compact IRI: `xsd:decimal`, `xsd:boolean`,
    /// `xsd:date`, `xsd:time` or `xsd:dateTime`.
    #[serde(rename = "type")]
    pub datatype: &'static str,
    /// The value's lexical form in that datatype: `0.12345`, `true`,
    /// `2023-07-16T18:00:00`.
    pub value: String,
}

impl Datum {
    pub fn new(datatype: &'static str, value: impl Into<String>) -> Self {
        Datum {
            datatype,
            value: value.into(),
        }
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

/// What a document says about itself: its title, who made it, and when.
///
/// Declared, never inferred: a PDF's Info dictionary, an Office file's core
/// properties, an HTML page's `<title>`, an email's headers. A guess at a
/// PDF's title from its largest line would be a reading of the page, and
/// that belongs in the elements. What a file declares can still be stale —
/// a title of `Microsoft Word - draft3.doc` is what that file says.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DocumentInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// As the document names them: a display name, or `Name <address>`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub creators: Vec<String>,
    /// ISO 8601. An email's is when it was sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

impl DocumentInfo {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.creators.is_empty()
            && self.created.is_none()
            && self.modified.is_none()
    }
}

/// A W3C date, time or date-time as XML Schema writes it, with the XSD
/// datatype it is — `2026-07-17` an `xsd:date`, `13:48:05Z` an `xsd:time`,
/// `2026-07-17T13:48Z` the `xsd:dateTime` `2026-07-17T13:48:00Z` — or `None`
/// when it is not a real one.
///
/// A value typed as one of these that is not one fails a validating store's
/// whole insert, and a file's dates are whatever its writer put there. So
/// they are checked rather than passed on: no day that does not exist, no
/// zone beyond ±14:00, and the seconds XML Schema requires and a W3C
/// date-time may leave out. Nothing but ASCII is a date.
pub fn xsd_temporal(s: &str) -> Option<(&'static str, String)> {
    let s = s.trim();
    if !s.is_ascii() {
        return None;
    }
    if let Some(date) = s.get(..10).filter(|d| is_date(d)) {
        let rest = &s[10..];
        return match rest.strip_prefix('T') {
            Some(time) => Some(("xsd:dateTime", format!("{date}T{}", clock(time)?))),
            None => Some(("xsd:date", format!("{date}{}", zone(rest)?))),
        };
    }
    Some(("xsd:time", clock(s)?))
}

/// [`xsd_temporal`] for a date or a date-time, as a document dates itself.
pub fn xsd_date_time(s: &str) -> Option<String> {
    xsd_temporal(s)
        .filter(|(ty, _)| *ty != "xsd:time")
        .map(|(_, v)| v)
}

/// Exactly `n` ASCII digits, as a number.
fn digits(t: &str, n: usize) -> Option<u32> {
    (t.len() == n && t.bytes().all(|b| b.is_ascii_digit()))
        .then(|| t.parse().ok())
        .flatten()
}

/// `YYYY-MM-DD`, ten ASCII bytes, naming a day that exists.
fn is_date(d: &str) -> bool {
    let (Some(y), Some(m), Some(day)) = (
        digits(&d[0..4], 4),
        digits(&d[5..7], 2),
        digits(&d[8..10], 2),
    ) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    &d[4..5] == "-" && &d[7..8] == "-" && (1..=days).contains(&day)
}

/// `hh:mm`, `hh:mm:ss` or `hh:mm:ss.f…`, then a [`zone`], as XML Schema
/// writes it: with its seconds.
fn clock(t: &str) -> Option<String> {
    let (time, tz) = t.split_at(t.find(['Z', '+', '-']).unwrap_or(t.len()));
    let tz = zone(tz)?;
    let mut parts = time.splitn(3, ':');
    let (hh, mm) = (parts.next()?, parts.next()?);
    let sec = parts.next().unwrap_or("00");
    let (whole, frac) = match sec.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (sec, None),
    };
    let ok = digits(hh, 2).is_some_and(|h| h <= 23)
        && digits(mm, 2).is_some_and(|m| m <= 59)
        && digits(whole, 2).is_some_and(|s| s <= 59)
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| format!("{hh}:{mm}:{sec}{tz}"))
}

/// Nothing, `Z`, or `±hh:mm` no further than 14 hours from UTC.
fn zone(z: &str) -> Option<&str> {
    match z.as_bytes() {
        [] | [b'Z'] => Some(z),
        [b'+' | b'-', _, _, b':', _, _] => {
            let (h, m) = (digits(&z[1..3], 2)?, digits(&z[4..6], 2)?);
            (h < 14 && m < 60 || h == 14 && m == 0).then_some(z)
        }
        _ => None,
    }
}

/// A file carried inside a document, described.
///
/// Only described: an attachment is a document of its own, which its own
/// reader reads, and its bytes are the parsing reader's to hand back beside
/// the elements. What the element model keeps is that it is there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// The MIME type the document declares for it.
    pub content_type: String,
    /// In bytes, decoded.
    pub size: usize,
    /// Its decoded bytes as lowercase hex SHA-256: the same file sent in
    /// fifty messages, under any name, is one file.
    pub sha256: String,
    /// Shown in the body, as an image pasted into a message is, rather than
    /// attached to it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inline: bool,
}

/// Facts about a document that are not elements of it.
///
/// Additive: the emitters take this where they can carry it, and their
/// existing signatures stay valid with an empty one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notes {
    pub unread: Vec<UnreadPage>,
    /// Running header and footer text, kept once because it identifies the
    /// document even though it is noise inside the body.
    pub running_text: Vec<String>,
    pub info: DocumentInfo,
    pub attachments: Vec<Attachment>,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.unread.is_empty()
            && self.running_text.is_empty()
            && self.info.is_empty()
            && self.attachments.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_passed_on_only_as_a_real_one() {
        let ok = |s: &str| xsd_date_time(s);
        assert_eq!(ok("2026-07-17").as_deref(), Some("2026-07-17"));
        assert_eq!(
            ok("2026-07-17T13:48Z").as_deref(),
            Some("2026-07-17T13:48:00Z")
        );
        assert_eq!(
            ok("2026-07-17T13:48:05.25+02:00").as_deref(),
            Some("2026-07-17T13:48:05.25+02:00")
        );
        assert_eq!(
            ok("2024-02-29T00:00:00").as_deref(),
            Some("2024-02-29T00:00:00")
        );
        for bad in [
            "2023-02-29",
            "2026-13-01",
            "2026-04-31T10:00:00",
            "2026-07-17T25:00:00",
            "2026-07-17Tnoon",
            "2026-07-17T10:00:00+5",
            "2026-07-17T1:00:00",
            "2026-07-17T10:5",
            "2026-07-17T10:00:00.",
            "2026-7-17",
            "sometime",
            // Beyond ±14:00, and not ASCII where a zone's digits go.
            "2026-07-17T10:00:00+14:59",
            "2026-07-17T10:00:00+15:00",
            "2026-07-17T10:00:00+0\u{e9}00",
            "2026-07-17T10:00:00+\u{e9}:00",
            "\u{e9}026-07-17",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
        assert_eq!(
            ok("2026-07-17T10:00:00-14:00").as_deref(),
            Some("2026-07-17T10:00:00-14:00")
        );
    }

    #[test]
    fn a_temporal_value_is_typed_by_what_it_is() {
        let t = |s: &str| xsd_temporal(s);
        let is = |ty: &'static str, v: &str| Some((ty, v.to_string()));
        assert_eq!(t("2026-07-17"), is("xsd:date", "2026-07-17"));
        // A date with a zone is a date, colon or not.
        assert_eq!(t("2026-07-17+02:00"), is("xsd:date", "2026-07-17+02:00"));
        assert_eq!(
            t("2026-07-17T10:00"),
            is("xsd:dateTime", "2026-07-17T10:00:00")
        );
        assert_eq!(t("13:48:05Z"), is("xsd:time", "13:48:05Z"));
        assert_eq!(t("2026-02-30"), None);
        assert_eq!(t("2026-07-17+2"), None);
        // A time is not a date a document can be dated by.
        assert_eq!(xsd_date_time("13:48:05"), None);
    }
}
