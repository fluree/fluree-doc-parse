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

    /// What an Office file declares in its core properties part
    /// (`docProps/core.xml` in a `.docx`, `.pptx` or `.xlsx`): `dc:title`,
    /// `dc:creator`, and `dcterms:created` / `dcterms:modified`.
    ///
    /// The part is a flat list of elements holding text, so it is read as
    /// one, by local name: any prefix a writer chose works. A date that is
    /// not ISO 8601 is left out rather than passed on as one.
    pub fn from_core_properties(xml: &str) -> Self {
        let mut info = DocumentInfo::default();
        let mut rest = xml;
        while let Some(open) = rest.find('<') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('>') else { break };
            let tag = &rest[..close];
            rest = &rest[close + 1..];
            if tag.starts_with(['/', '?', '!']) || tag.ends_with('/') {
                continue;
            }
            let name = tag.split_whitespace().next().unwrap_or("");
            let local = name.rsplit(':').next().unwrap_or(name);
            let text = unescape_xml(&rest[..rest.find('<').unwrap_or(rest.len())]);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            match local {
                "title" => info.title = Some(text.to_string()),
                "creator" => info.creators.push(text.to_string()),
                "created" => info.created = xsd_date_time(text),
                "modified" => info.modified = xsd_date_time(text),
                _ => {}
            }
        }
        info
    }
}

/// The five named entities and character references, as XML text holds them.
fn unescape_xml(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';') else { break };
        let entity = &rest[1..semi];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .map(|h| u32::from_str_radix(h, 16))
                .or_else(|| entity.strip_prefix('#').map(str::parse::<u32>))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A W3C date or date-time (`2026-07-17`, `2026-07-17T13:48Z`,
/// `2026-07-17T13:48:00.5+02:00`) as XML Schema writes it, or `None` when it
/// is not a real one.
///
/// Dates on the document node are typed `xsd:date` and `xsd:dateTime`, and a
/// store that checks the type rejects the whole insert over one that is
/// not: XML Schema needs the seconds a W3C date-time may leave out, and no
/// day that does not exist. A file's dates are whatever its writer put
/// there, so they are checked rather than passed on.
pub fn xsd_date_time(s: &str) -> Option<String> {
    let num = |t: &str| -> Option<u32> {
        t.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| t.parse().ok())
            .flatten()
    };
    let s = s.trim();
    let (y, m, d) = (num(s.get(0..4)?)?, num(s.get(5..7)?)?, num(s.get(8..10)?)?);
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if &s[4..5] != "-" || &s[7..8] != "-" || d == 0 || d > days {
        return None;
    }
    let date = &s[..10];
    let Some(time) = s[10..].strip_prefix('T') else {
        return s[10..].is_empty().then(|| date.to_string());
    };
    // The zone: `Z`, `+hh:mm` or `-hh:mm` at the end, or nothing.
    let (clock, zone) = match time.find(['Z', '+', '-']) {
        Some(i) => time.split_at(i),
        None => (time, ""),
    };
    let zone_ok = match zone.as_bytes() {
        [] | [b'Z'] => true,
        [b'+' | b'-', ..] => {
            zone.len() == 6
                && &zone[3..4] == ":"
                && num(&zone[1..3]).is_some_and(|h| h <= 14)
                && num(&zone[4..6]).is_some_and(|m| m < 60)
        }
        _ => false,
    };
    let mut parts = clock.splitn(3, ':');
    let (hh, mm) = (parts.next()?, parts.next()?);
    let sec = parts.next().unwrap_or("00");
    let (whole, frac) = match sec.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (sec, None),
    };
    let two = |t: &str, max: u32| t.len() == 2 && num(t).is_some_and(|v| v <= max);
    let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    (zone_ok && two(hh, 23) && two(mm, 59) && two(whole, 59) && frac_ok)
        .then(|| format!("{date}T{hh}:{mm}:{sec}{zone}"))
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
    fn core_properties_are_read_by_local_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
<dc:title>Q3 &amp; Q4 Plan &#8212; Draft</dc:title><dc:subject/><dc:creator>Ada Park</dc:creator>
<cp:lastModifiedBy>Kai Moreno</cp:lastModifiedBy>
<dcterms:created xsi:type="dcterms:W3CDTF">2026-07-17T13:48:00Z</dcterms:created>
<dcterms:modified xsi:type="dcterms:W3CDTF">sometime</dcterms:modified>
</cp:coreProperties>"#;
        let info = DocumentInfo::from_core_properties(xml);
        assert_eq!(info.title.as_deref(), Some("Q3 & Q4 Plan \u{2014} Draft"));
        assert_eq!(info.creators, ["Ada Park"]);
        assert_eq!(info.created.as_deref(), Some("2026-07-17T13:48:00Z"));
        assert_eq!(info.modified, None, "not a date, so not passed on as one");
        assert!(DocumentInfo::from_core_properties("<cp:coreProperties/>").is_empty());
    }

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
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }
}
