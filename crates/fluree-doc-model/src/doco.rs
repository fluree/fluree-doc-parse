//! Direct DoCO JSON-LD emission — no XHTML round-trip.
//!
//! The downstream document workflow historically consumed XHTML and walked
//! its tags back into a DoCO (Document Components Ontology) JSON-LD graph:
//! headings were serialized to `<h2>` only so a walker could re-derive the
//! section tree from tag names, and everything the tags could not carry —
//! page, bounding box, classification evidence — was destroyed in transit.
//! This emitter produces that graph directly from the element stream, same
//! shape the XHTML walker builds, with the spatial provenance kept:
//!
//! - `@graph` of elements in reading order; hierarchy lives only in
//!   `po:contains` (coerced to `@id` in the context so ingestion links
//!   rather than storing dead strings).
//! - A `doco:Document` root containing `doco:BodyMatter`; headings open a
//!   `doco:Section` (with `doc:sectionLevel`) holding a `doco:SectionTitle`,
//!   and content attaches to the innermost open section — the same stack
//!   walk the XHTML consumer performs on `h1`-`h6`.
//! - Consecutive list items group under a `doco:List`; tables carry
//!   `doc:TableCell` children with row/column indices, header labels,
//!   and cell values. Cells whose text appears verbatim in the projection
//!   also carry `nif:beginIndex`/`nif:endIndex` (and `nif:isString`), so a
//!   consumer can scope work to a table *row* instead of the whole table;
//!   a value synthesised by merge denormalisation has no place in the
//!   projection and carries no offsets rather than wrong ones.
//! - Text rides on `nif:isString`, with a display `rdfs:label` truncated to
//!   [`LABEL_MAX_CHARS`]; `nif:beginIndex`/`nif:endIndex` are character
//!   offsets into the plain-text projection ([`to_text`]) so entity mentions
//!   located in that projection join to their element by interval lookup.
//! - Every element additionally carries `doc:pageIndex` and `doc:bbox`
//!   (`"x0,y0,x1,y1"`, PDF units, top-left origin) — the fields the XHTML
//!   round-trip could not represent. `doc:pageIndex` is the 0-based physical
//!   position in the page sequence, deliberately not the printed page number:
//!   front matter, unnumbered inserts, and roman-numeral folios make the
//!   printed label a document-authored string that may be absent or repeat,
//!   while the physical index is always defined and always unique. For
//!   paginationless sources it is the slide or sheet index.

use crate::element::{
    Attachment, DocumentInfo, Element, Property, PropertyValue, SourceField, Target,
};
use crate::message::{Mailbox, Message};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

/// Display-label budget: full text lives in `nif:isString`, the label is a
/// preview for graph canvases.
const LABEL_MAX_CHARS: usize = 100;

#[derive(Default)]
pub struct DocoOptions {
    /// Prefix for minted element IRIs: `{base_iri}/section/{n}` and
    /// `{base_iri}/element/{n}`, one shared counter in emission order.
    pub base_iri: String,
    /// When present, every element is stamped `doc:sourceDocument → {iri}` —
    /// the retract-on-rerun tag a re-extraction targets.
    pub doc_iri: Option<String>,
    /// The document's running header and footer text, emitted once on the
    /// document node.
    ///
    /// Furniture is removed from the body because a line repeating on every
    /// page is noise *there*. It is not noise about the document: a
    /// controlled procedure puts its owner, title and number in exactly that
    /// block, and a three-page one repeats them on all three, so stripping
    /// the repetition strips the identity. Declared here rather than kept in
    /// the text, which would put it back into the body it was removed from
    /// and shift every character offset in the graph.
    pub running_text: Vec<String>,
    /// Pages carrying content nothing transcribed. Emitted on the document
    /// node as `doc:unreadPages`, so a consumer can tell an empty page from a
    /// page that was not read.
    pub unread: Vec<crate::element::UnreadPage>,
    /// Page sizes, in PDF user units, for sources that have pages.
    ///
    /// Emitted on the document node as `doc:pages`. A `doc:bbox` cannot be
    /// placed on a rendered page without them: the consumer needs the ratio
    /// between the page's own units and the pixels it rendered to, and this
    /// is the only place the denominator appears. Empty for sources with no
    /// geometry.
    pub pages: Vec<crate::geom::PageSize>,
    /// What the document says about itself, emitted on the document node in
    /// Dublin Core terms.
    pub info: DocumentInfo,
    /// Files the document carries. Described on the document node as
    /// `doc:attachments`; their content is theirs to convert.
    pub attachments: Vec<Attachment>,
    /// The fields of the record the document was read from, emitted on the
    /// document node as `doc:sourceFields`.
    ///
    /// The declared properties are statements of the graph, and a consumer
    /// asks the graph for them. This is the record as the source held it,
    /// order and unmapped fields included, which no set of statements keeps.
    pub fields: Vec<SourceField>,
}

/// The plain-text projection: each text-bearing element's trimmed text in
/// reading order, separated by blank lines. `nif:beginIndex`/`nif:endIndex`
/// in [`to_doco`] index into exactly this string (in characters), so the two
/// outputs form a contract: locate a mention in the text, look up its
/// element by interval.
pub fn to_text(elements: &[Element]) -> String {
    let mut out = String::new();
    for e in elements {
        let t = projection_text(e);
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&t);
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// An element's contribution to the text projection.
///
/// Public because anything mapping a projection offset back to a position has
/// to walk the projection the same way [`to_text`] builds it. Re-deriving it
/// from `Element::text` gets tables wrong — their contribution is the cells
/// joined with tabs — and the resulting offsets drift by exactly the
/// difference, silently.
pub fn projection_text(e: &Element) -> String {
    match (&e.cells, e.kind.as_str()) {
        (Some(rows), "doco:Table") => table_projection(rows).0,
        (None, "doco:Table") => strip_tags(&e.text).trim().to_string(),
        _ => e.text.trim().to_string(),
    }
}

/// A table's projection text together with each non-empty cell's char span
/// within it.
///
/// One function builds both because they must agree exactly: the projection
/// is rows of trimmed cells tab-joined (row trailing whitespace dropped,
/// rows newline-joined, the whole trimmed), and a span computed by any
/// second walk drifts from it silently. Spans are keyed `(row, column)` in
/// the RAW grid — the same indices the cell-emission loop iterates — and
/// exist only for cells whose trimmed text is non-empty, i.e. the cells
/// that actually appear in the projection.
pub fn table_projection(
    rows: &[Vec<String>],
) -> (String, BTreeMap<(usize, usize), (usize, usize)>) {
    let mut out = String::new();
    let mut cursor = 0usize; // chars, not bytes
    let mut spans: BTreeMap<(usize, usize), (usize, usize)> = BTreeMap::new();
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            out.push('\n');
            cursor += 1;
        }
        let mut row_str = String::new();
        let mut row_cursor = 0usize;
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                row_str.push('\t');
                row_cursor += 1;
            }
            let t = cell.trim();
            if !t.is_empty() {
                let len = t.chars().count();
                spans.insert((r, c), (cursor + row_cursor, cursor + row_cursor + len));
                row_cursor += len;
            }
            row_str.push_str(t);
        }
        // Trailing empty cells left only tabs; drop them. Spans are
        // unaffected — a non-empty cell's text is never trailing
        // whitespace.
        let trimmed_row = row_str.trim_end();
        out.push_str(trimmed_row);
        cursor += trimmed_row.chars().count();
    }
    // The final trim: leading whitespace exists only when leading rows
    // were entirely empty, so surviving spans just shift left by the
    // trimmed char count.
    let lead = out.chars().count() - out.trim_start().chars().count();
    let text = out.trim().to_string();
    if lead > 0 {
        let shifted = spans
            .into_iter()
            .map(|(k, (b, e))| (k, (b - lead, e - lead)))
            .collect();
        return (text, shifted);
    }
    (text, spans)
}

/// Minimal tag stripper for model-arbitrated tables whose text is HTML.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Emitter<'a> {
    opts: &'a DocoOptions,
    nodes: Vec<Map<String, Value>>,
    /// node index → ordered child IRIs (kept separate so `po:contains` can be
    /// attached in one pass at the end).
    children: Vec<Vec<String>>,
    counter: usize,
    /// (id-index of open section, its level)
    open_sections: Vec<(usize, usize)>,
    /// The `doc:Message` node the current email message's elements go into.
    open_message: Option<usize>,
    /// Mailbox IRIs by address (lower-cased), or by name for a mailbox
    /// written without one.
    mailboxes: HashMap<String, String>,
    body_idx: usize,
}

impl<'a> Emitter<'a> {
    fn mint(&mut self, kind_segment: &str) -> String {
        let iri = format!("{}/{}/{}", self.opts.base_iri, kind_segment, self.counter);
        self.counter += 1;
        iri
    }

    /// Create a node, returning its index.
    fn node(&mut self, segment: &str, doco_type: &str, tag: Option<&str>) -> usize {
        let iri = self.mint(segment);
        let mut m = Map::new();
        m.insert("@id".into(), Value::String(iri));
        m.insert("@type".into(), Value::String(doco_type.into()));
        if let Some(tag) = tag {
            m.insert("doc:xhtmlTag".into(), Value::String(tag.into()));
        }
        if let Some(doc_iri) = &self.opts.doc_iri {
            m.insert("doc:sourceDocument".into(), json!({ "@id": doc_iri }));
        }
        self.nodes.push(m);
        self.children.push(Vec::new());
        self.nodes.len() - 1
    }

    fn attach(&mut self, parent: usize, child: usize) {
        let child_iri = self.nodes[child]["@id"].as_str().unwrap().to_string();
        self.children[parent].push(child_iri);
    }

    /// The innermost open section, else the open message, else the body.
    fn current_parent(&self) -> usize {
        self.open_sections
            .last()
            .map(|(i, _)| *i)
            .or(self.open_message)
            .unwrap_or(self.body_idx)
    }

    /// Mint a `doc:Mailbox` node for every sender and recipient in the
    /// document, one per address.
    ///
    /// A node rather than a string on the message, so that a person can be
    /// found by address: every message they sent or received in this
    /// document points at the same node, and `doc:address` is the value a
    /// contact record joins on. Minted per document, like every other node
    /// here, so re-extracting one document never touches another's. A
    /// mailbox written with a name alone, as a quoted Outlook header often
    /// is, joins the addressed mailbox of the same name when there is one.
    fn register_mailboxes(&mut self, elements: &[Element]) {
        let all: Vec<Mailbox> = elements
            .iter()
            .filter_map(|e| e.message.as_deref())
            .flat_map(|m| m.from.iter().chain(&m.to).chain(&m.cc).chain(&m.bcc))
            .cloned()
            .collect();
        let (addressed, named): (Vec<&Mailbox>, Vec<&Mailbox>) =
            all.iter().partition(|m| m.address.is_some());
        for m in addressed.into_iter().chain(named) {
            let key = mailbox_key(m);
            if key.is_empty() || self.mailboxes.contains_key(&key) {
                continue;
            }
            if m.address.is_none() {
                if let Some(iri) = m.name.as_deref().and_then(|n| self.mailbox_by_name(n)) {
                    self.mailboxes.insert(key, iri);
                    continue;
                }
            }
            let idx = self.node("mailbox", "doc:Mailbox", None);
            if let Some(a) = &m.address {
                self.nodes[idx].insert("doc:address".into(), Value::String(a.clone()));
            }
            if let Some(n) = &m.name {
                self.nodes[idx].insert("doc:name".into(), Value::String(n.clone()));
            }
            let iri = self.nodes[idx]["@id"].as_str().unwrap().to_string();
            self.mailboxes.insert(key, iri);
        }
    }

    fn mailbox_by_name(&self, name: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| {
                n["@type"] == "doc:Mailbox"
                    && n.get("doc:address").is_some()
                    && n.get("doc:name")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v.eq_ignore_ascii_case(name))
            })
            .map(|n| n["@id"].as_str().unwrap().to_string())
    }

    /// Open a `doc:Message` for an element that starts one, and describe it.
    ///
    /// Messages sit side by side under the body, in reading order, rather
    /// than nested the way the quoting nests them: a thread is a sequence
    /// of messages, each by its own sender, and "everything Ada wrote" is
    /// then one step from each of them. Sections a message's own headings
    /// open stay inside it.
    fn open_message(&mut self, m: &Message) {
        self.open_sections.clear();
        let idx = self.node("message", "doc:Message", None);
        for (key, boxes) in [
            ("doc:from", &m.from),
            ("doc:to", &m.to),
            ("doc:cc", &m.cc),
            ("doc:bcc", &m.bcc),
        ] {
            let iris: Vec<Value> = boxes
                .iter()
                .filter_map(|b| self.mailboxes.get(&mailbox_key(b)).cloned())
                .map(Value::String)
                .collect();
            if !iris.is_empty() {
                self.nodes[idx].insert(key.into(), Value::Array(iris));
            }
        }
        if let Some(d) = &m.date {
            self.nodes[idx].insert("doc:sentAt".into(), date_time(d));
        }
        if let Some(s) = &m.subject {
            self.nodes[idx].insert("doc:subject".into(), Value::String(s.clone()));
        }
        if let Some(id) = &m.message_id {
            self.nodes[idx].insert("doc:messageId".into(), Value::String(id.clone()));
        }
        for (key, ids) in [
            ("doc:inReplyTo", &m.in_reply_to),
            ("doc:references", &m.references),
        ] {
            if !ids.is_empty() {
                self.nodes[idx].insert(key.into(), json!(ids));
            }
        }
        if m.quoted {
            self.nodes[idx].insert("doc:quoted".into(), Value::Bool(true));
        }
        let body = self.body_idx;
        self.attach(body, idx);
        self.open_message = Some(idx);
    }

    fn set_text(&mut self, idx: usize, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        self.nodes[idx].insert("nif:isString".into(), Value::String(trimmed.into()));
        let label: String = if trimmed.chars().count() > LABEL_MAX_CHARS {
            let mut l: String = trimmed.chars().take(LABEL_MAX_CHARS).collect();
            l.push('\u{2026}');
            l
        } else {
            trimmed.into()
        };
        self.nodes[idx].insert("rdfs:label".into(), Value::String(label));
    }

    fn set_provenance(&mut self, idx: usize, e: &Element) {
        self.nodes[idx].insert("doc:pageIndex".into(), json!(e.page));
        // Only sources with layout carry a box. Emitting zeros for the rest
        // would hand every consumer a real-looking position.
        if let Some(b) = e.bbox {
            self.nodes[idx].insert(
                "doc:bbox".into(),
                Value::String(format!("{:.2},{:.2},{:.2},{:.2}", b.x0, b.y0, b.x1, b.y1)),
            );
        }
        self.nodes[idx].insert("doc:evidence".into(), Value::String(e.evidence.into()));
        // A recording places an element in time rather than on a page.
        // Plain integers, so "what was said between minute 4 and 5" is a
        // numeric comparison in any query language.
        if let Some(t) = &e.turn {
            if let Some(s) = &t.speaker {
                self.nodes[idx].insert("doc:speaker".into(), Value::String(s.clone()));
            }
            self.nodes[idx].insert("doc:startMs".into(), json!(t.start_ms));
            self.nodes[idx].insert("doc:endMs".into(), json!(t.end_ms));
        }
        // A record places an element in a field.
        if let Some(path) = &e.source_path {
            self.nodes[idx].insert("doc:sourcePath".into(), Value::String(path.clone()));
        }
    }

    fn set_offsets(&mut self, idx: usize, start: usize, end: usize) {
        self.nodes[idx].insert("nif:beginIndex".into(), json!(start));
        self.nodes[idx].insert("nif:endIndex".into(), json!(end));
    }

    /// Emit a `doc:Link` node per link on this element and reference them
    /// from it.
    ///
    /// A node rather than a property on the element, because an element can
    /// carry several links and each one has its own anchor: flattening them
    /// onto the element would leave a consumer with a set of targets and no
    /// way to tell which words point where.
    ///
    /// Anchor offsets land in the same space as every other offset in the
    /// graph — characters into the text projection — so the interval lookup
    /// that finds an entity mention finds a link anchor too. `text_start` is
    /// where this element's text begins in that projection; where the
    /// projection is not the element's own text (a table, whose cells are
    /// joined with tabs) it is `None`, and the link is emitted without
    /// offsets rather than with wrong ones.
    fn set_links(&mut self, idx: usize, e: &Element, text_start: Option<usize>) {
        let Some(links) = e.links.clone() else {
            return;
        };
        // `set_text` trims, so offsets into the projection have to trim with
        // it.
        let lead = e.text.chars().take_while(|c| c.is_whitespace()).count();
        let mut iris = Vec::new();
        for l in &links {
            let node = self.node("link", "doc:Link", None);
            match &l.target {
                Target::Uri { uri } => {
                    self.nodes[node].insert("doc:linkTarget".into(), json!({ "@id": uri }));
                }
                Target::Page { page } => {
                    self.nodes[node].insert("doc:linkPage".into(), json!(page));
                }
            }
            if let (Some(start), Some((b, end))) = (text_start, l.span()) {
                if b >= lead {
                    self.set_offsets(node, start + b - lead, start + end - lead);
                    let anchor: String = e.text.chars().skip(b).take(end - b).collect();
                    self.nodes[node].insert("nif:isString".into(), Value::String(anchor));
                }
            }
            iris.push(self.nodes[node]["@id"].as_str().unwrap().to_string());
        }
        if iris.is_empty() {
            return;
        }
        self.nodes[idx].insert(
            "doc:link".into(),
            Value::Array(iris.into_iter().map(Value::String).collect()),
        );
    }
}

/// The key a mailbox is minted under: its address, case folded, or its name
/// where it has no address.
fn mailbox_key(m: &Mailbox) -> String {
    match (&m.address, &m.name) {
        (Some(a), _) => a.to_lowercase(),
        (None, Some(n)) => format!("name:{}", n.to_lowercase()),
        (None, None) => String::new(),
    }
}

/// An ISO 8601 timestamp as a typed literal, so a store compares it as a
/// time rather than a string. A bare date is an `xsd:date`.
fn date_time(v: &str) -> Value {
    let ty = if v.contains('T') {
        "xsd:dateTime"
    } else {
        "xsd:date"
    };
    json!({ "@value": v, "@type": ty })
}

/// A typed literal's value as JSON writes that type: a number as a number,
/// a truth value as one, anything else as its text.
///
/// `"266000"` typed `xsd:int` is a string that claims to be a number. A
/// store takes it at its type and then finds a string where it keeps
/// numbers, and the statement it accepted is one it cannot read back. A
/// value that is not of its claimed type stays text, which is at least
/// what it is.
fn typed_value(value: &str, datatype: &str) -> Value {
    let Some(local) = datatype.strip_prefix("http://www.w3.org/2001/XMLSchema#") else {
        return Value::String(value.to_string());
    };
    let v = value.trim();
    let parsed = match local {
        "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
        | "positiveInteger" | "unsignedInt" | "unsignedLong" => {
            v.parse::<i64>().ok().map(Value::from)
        }
        "decimal" | "double" | "float" => v
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        "boolean" => match v {
            "true" | "1" => Some(Value::Bool(true)),
            "false" | "0" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    };
    parsed.unwrap_or_else(|| Value::String(value.to_string()))
}

/// State `properties` on `node`, each under its own IRI.
///
/// A property stated twice has two values, in the order they were stated,
/// and one when both say the same: a city given by its name and by its code
/// is one city.
/// A key the node already holds is left alone: the emitter's own statements
/// (`@id`, `@type`, the Dublin Core terms) are not a record's to overwrite,
/// and a property spelled as an absolute IRI never collides with their
/// prefixed names.
fn state_properties(node: &mut Map<String, Value>, properties: &[Property]) {
    let mut stated: Vec<(&str, Vec<Value>)> = Vec::new();
    for p in properties {
        let value = match &p.value {
            PropertyValue::Literal {
                value,
                datatype: Some(datatype),
            } => json!({ "@value": typed_value(value, datatype), "@type": datatype }),
            PropertyValue::Literal {
                value,
                datatype: None,
            } => Value::String(value.clone()),
            PropertyValue::Iri { iri } => json!({ "@id": iri }),
        };
        match stated.iter_mut().find(|(k, _)| *k == p.property) {
            Some((_, values)) if values.contains(&value) => {}
            Some((_, values)) => values.push(value),
            None => stated.push((&p.property, vec![value])),
        }
    }
    for (property, mut values) in stated {
        if node.contains_key(property) {
            continue;
        }
        let value = if values.len() == 1 {
            values.remove(0)
        } else {
            Value::Array(values)
        };
        node.insert(property.to_string(), value);
    }
}

pub fn to_doco(elements: &[Element], opts: &DocoOptions) -> String {
    let mut em = Emitter {
        opts,
        nodes: Vec::new(),
        children: Vec::new(),
        counter: 0,
        open_sections: Vec::new(),
        open_message: None,
        mailboxes: HashMap::new(),
        body_idx: 0,
    };

    let doc_idx = em.node("element", "doco:Document", Some("html"));
    if !opts.running_text.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:runningText".into(),
            json!({ "@type": "@json", "@value": opts.running_text }),
        );
    }
    if !opts.unread.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:unreadPages".into(),
            json!({ "@type": "@json", "@value": opts.unread }),
        );
    }
    if !opts.pages.is_empty() {
        // A JSON literal rather than a node per page: these are the page's
        // measurements, not things a graph should grow edges to.
        em.nodes[doc_idx].insert(
            "doc:pages".into(),
            json!({ "@type": "@json", "@value": opts.pages }),
        );
    }
    let info = &opts.info;
    if let Some(t) = &info.title {
        em.nodes[doc_idx].insert("dcterms:title".into(), Value::String(t.clone()));
    }
    if !info.creators.is_empty() {
        em.nodes[doc_idx].insert("dcterms:creator".into(), json!(info.creators));
    }
    for (key, value) in [
        ("dcterms:created", &info.created),
        ("dcterms:modified", &info.modified),
    ] {
        if let Some(v) = value {
            em.nodes[doc_idx].insert(key.into(), date_time(v));
        }
    }
    if !opts.attachments.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:attachments".into(),
            json!({ "@type": "@json", "@value": opts.attachments }),
        );
    }
    if let Some(class) = &info.class {
        // Beside `doco:Document`, not instead of it: the structure is a
        // document's whatever the record describes.
        em.nodes[doc_idx].insert("@type".into(), json!(["doco:Document", class]));
    }
    state_properties(&mut em.nodes[doc_idx], &info.properties);
    if !opts.fields.is_empty() {
        em.nodes[doc_idx].insert(
            "doc:sourceFields".into(),
            json!({ "@type": "@json", "@value": opts.fields }),
        );
    }
    let body_idx = em.node("element", "doco:BodyMatter", Some("body"));
    em.body_idx = body_idx;
    em.attach(doc_idx, body_idx);
    em.register_mailboxes(elements);

    // Character cursor into the `to_text` projection.
    let mut cursor = 0usize;
    let mut first_text = true;
    let mut offsets_for = |text: &str| -> (usize, usize) {
        if !first_text {
            cursor += 2; // the "\n\n" separator
        }
        first_text = false;
        let start = cursor;
        cursor += text.chars().count();
        (start, cursor)
    };

    let mut open_list: Option<usize> = None;
    for e in elements {
        if open_list.is_some() && e.kind != "doco:ListItem" {
            open_list = None;
        }
        if let Some(m) = &e.message {
            open_list = None;
            em.open_message(m);
        }
        match e.kind.as_str() {
            "doco:SectionTitle" => {
                let level = e.level.unwrap_or(1).clamp(1, 6);
                while em.open_sections.last().is_some_and(|(_, l)| *l >= level) {
                    em.open_sections.pop();
                }
                let parent = em.current_parent();
                let section = em.node("section", "doco:Section", None);
                em.nodes[section].insert("doc:sectionLevel".into(), json!(level));
                em.attach(parent, section);

                let title = em.node("element", "doco:SectionTitle", Some(&format!("h{level}")));
                let text = projection_text(e);
                em.set_text(title, &text);
                em.set_provenance(title, e);
                let mut start = None;
                if !text.is_empty() {
                    let (s, t) = offsets_for(&text);
                    em.set_offsets(title, s, t);
                    start = Some(s);
                }
                em.set_links(title, e, start);
                em.attach(section, title);
                em.open_sections.push((section, level));
            }
            "doco:ListItem" => {
                let list = match open_list {
                    Some(l) => l,
                    None => {
                        let parent = em.current_parent();
                        let l = em.node("element", "doco:List", Some("ul"));
                        em.attach(parent, l);
                        open_list = Some(l);
                        l
                    }
                };
                let item = em.node("element", "doco:ListItem", Some("li"));
                let text = projection_text(e);
                em.set_text(item, &text);
                em.set_provenance(item, e);
                let mut start = None;
                if !text.is_empty() {
                    let (s, t) = offsets_for(&text);
                    em.set_offsets(item, s, t);
                    start = Some(s);
                }
                em.set_links(item, e, start);
                em.attach(list, item);
            }
            "doco:Table" => {
                let parent = em.current_parent();
                let table = em.node("element", "doco:Table", Some("table"));
                let text = projection_text(e);
                em.set_text(table, &text);
                em.set_provenance(table, e);
                let mut table_start = None;
                if !text.is_empty() {
                    let (s, t) = offsets_for(&text);
                    em.set_offsets(table, s, t);
                    table_start = Some(s);
                }
                // A table's projection joins cells with tabs, so an anchor
                // offset into the element's own text indexes nothing here.
                em.set_links(table, e, None);
                em.attach(parent, table);

                // Cell entities: data rows index from 0 below the measured
                // header (`header_rows` — absent means undetected, treated
                // as the presumed single header row). Column names come from
                // the *last* header row: rows above it in a stacked header
                // are spanning banners, not column labels. Empty cells are
                // not materialised.
                if let Some(raw_rows) = &e.cells {
                    // Where each raw cell's text sits inside the table's
                    // projection block — the anchor for per-cell offsets.
                    let cell_spans = table_projection(raw_rows).1;
                    // Each cell becomes an entity that must describe itself,
                    // so merged values are filled down here — a cell blanked
                    // by the rowspan convention would otherwise lose its row
                    // context entirely.
                    let mut rows = raw_rows.clone();
                    if e.merged_down.is_some() || e.merged_left.is_some() || e.sub_headers.is_some()
                    {
                        let ncols = rows.first().map(Vec::len).unwrap_or(0);
                        let m = crate::merges::Merges {
                            continues_above: e
                                .merged_down
                                .clone()
                                .unwrap_or_else(|| vec![false; rows.len() * ncols]),
                            continues_left: e
                                .merged_left
                                .clone()
                                .unwrap_or_else(|| vec![false; rows.len() * ncols]),
                            full_width_row: (0..rows.len())
                                .map(|r| e.sub_headers.as_ref().is_some_and(|s| s.contains(&r)))
                                .collect(),
                        };
                        crate::merges::denormalize(&mut rows, &m);
                    }
                    let rows = &rows;
                    let n_header = e.header_rows.unwrap_or(1).min(rows.len());
                    let header: Option<&Vec<String>> = (n_header > 0).then(|| &rows[n_header - 1]);
                    let subs = e.sub_headers.clone().unwrap_or_default();
                    for (r, row) in rows.iter().enumerate().skip(n_header) {
                        // A sub-header band labels the rows beneath it; it is
                        // not itself data. Its text rides on those rows as
                        // `doc:sectionLabel` so every cell entity carries
                        // the section it belongs to.
                        if subs.contains(&r) {
                            continue;
                        }
                        let section = subs
                            .iter()
                            .filter(|s| **s < r)
                            .max()
                            .and_then(|s| rows[*s].first())
                            .map(|t| t.trim())
                            .filter(|t| !t.is_empty());
                        let row_header = row.first().map(|c| c.trim()).unwrap_or("");
                        for (c, cell) in row.iter().enumerate() {
                            let value = cell.trim();
                            if value.is_empty() {
                                continue;
                            }
                            let cell_idx = em.node("element", "doc:TableCell", None);
                            em.nodes[cell_idx].insert("doc:rowIndex".into(), json!(r - n_header));
                            em.nodes[cell_idx].insert("doc:columnIndex".into(), json!(c));
                            if let Some(h) = header
                                .and_then(|h| h.get(c))
                                .map(|h| h.trim())
                                .filter(|h| !h.is_empty())
                            {
                                em.nodes[cell_idx]
                                    .insert("doc:columnHeader".into(), Value::String(h.into()));
                            }
                            if let Some(s) = section {
                                em.nodes[cell_idx]
                                    .insert("doc:sectionLabel".into(), Value::String(s.into()));
                            }
                            if c > 0 && !row_header.is_empty() {
                                em.nodes[cell_idx].insert(
                                    "doc:rowHeader".into(),
                                    Value::String(row_header.into()),
                                );
                            }
                            em.nodes[cell_idx]
                                .insert("doc:cellValue".into(), Value::String(value.into()));
                            // Offsets only when the projection really shows
                            // this value at (r, c): merge denormalisation can
                            // synthesise or rejoin values (fill-down copies,
                            // fragment joins), and those have no span to
                            // point at. `nif:isString` rides along so the
                            // offset invariant — a span slices the
                            // projection to the node's text — holds for
                            // cells exactly as for every other node.
                            if let (Some(s), Some(&(b, t))) = (table_start, cell_spans.get(&(r, c)))
                            {
                                let raw =
                                    raw_rows.get(r).and_then(|row| row.get(c)).map(|c| c.trim());
                                if raw == Some(value) {
                                    em.set_offsets(cell_idx, s + b, s + t);
                                    em.nodes[cell_idx]
                                        .insert("nif:isString".into(), Value::String(value.into()));
                                }
                            }
                            em.attach(table, cell_idx);
                        }
                    }
                }
            }
            // A figure carries its fragments as an unordered set. The
            // sequence they were drawn in is not a reading of the chart --
            // pairing J&J's donut by that order attaches two of four regions
            // to the wrong percentage -- so `doc:figureContent` states what
            // is inside the region without asserting how the parts relate.
            // A consumer wanting the pairing needs the drawing, which is why
            // the bbox travels with it.
            "doco:Figure" => {
                let parent = em.current_parent();
                let f = em.node("element", "doco:Figure", Some("figure"));
                let text = projection_text(e);
                em.set_text(f, &text);
                em.set_provenance(f, e);
                let mut start = None;
                if !text.is_empty() {
                    let (s, t) = offsets_for(&text);
                    em.set_offsets(f, s, t);
                    start = Some(s);
                }
                em.set_links(f, e, start);
                if let Some(id) = &e.figure {
                    em.nodes[f].insert("doc:figure".into(), Value::String(id.clone()));
                }
                em.attach(parent, f);
            }
            // doco:Paragraph and anything else text-bearing.
            _ => {
                let parent = em.current_parent();
                let p = em.node("element", &e.kind, Some("p"));
                let text = projection_text(e);
                em.set_text(p, &text);
                em.set_provenance(p, e);
                let mut start = None;
                if !text.is_empty() {
                    let (s, t) = offsets_for(&text);
                    em.set_offsets(p, s, t);
                    start = Some(s);
                }
                em.set_links(p, e, start);
                em.attach(parent, p);
            }
        }
    }

    // Attach po:contains and build the graph.
    let Emitter {
        nodes, children, ..
    } = em;
    let graph: Vec<Value> = nodes
        .into_iter()
        .zip(children)
        .map(|(mut n, kids)| {
            if !kids.is_empty() {
                n.insert(
                    "po:contains".into(),
                    Value::Array(kids.into_iter().map(Value::String).collect()),
                );
            }
            Value::Object(n)
        })
        .collect();

    let doc = json!({
        "@context": {
            "doco": "http://purl.org/spar/doco/",
            "po": "http://www.essepuntato.it/2008/12/pattern#",
            "rdfs": "http://www.w3.org/2000/01/rdf-schema#",
            "nif": "http://persistence.uni-leipzig.org/nlp2rdf/ontologies/nif-core#",
            "doc": "https://ns.flur.ee/doc#",
            "dcterms": "http://purl.org/dc/terms/",
            "xsd": "http://www.w3.org/2001/XMLSchema#",
            // Containment edges must ingest as IRI references, not literals.
            "po:contains": { "@type": "@id" },
            // A link's anchor is a node of the graph, and its target is an
            // address rather than a string about one.
            "doc:link": { "@type": "@id" },
            "doc:linkTarget": { "@type": "@id" },
            // A message's sender and recipients are the graph's mailbox
            // nodes.
            "doc:from": { "@type": "@id" },
            "doc:to": { "@type": "@id" },
            "doc:cc": { "@type": "@id" },
            "doc:bcc": { "@type": "@id" },
        },
        "@graph": graph,
    });
    serde_json::to_string_pretty(&doc).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::BBox;

    fn el(kind: &str, text: &str, level: Option<usize>) -> Element {
        Element {
            id: String::new(),
            kind: kind.into(),
            page: 0,
            bbox: Some(BBox {
                x0: 10.0,
                y0: 20.0,
                x1: 110.0,
                y1: 40.0,
            }),
            text: text.into(),
            level,
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            turn: None,
            message: None,
            source_path: None,
            provenance: "rust",
            evidence: "layout",
        }
    }

    fn opts() -> DocoOptions {
        DocoOptions {
            base_iri: "urn:test".into(),
            doc_iri: Some("urn:test:doc".into()),
            pages: Vec::new(),
            unread: Vec::new(),
            running_text: Vec::new(),
            ..Default::default()
        }
    }

    fn graph(elements: &[Element]) -> Vec<Value> {
        let v: Value = serde_json::from_str(&to_doco(elements, &opts())).unwrap();
        v["@graph"].as_array().unwrap().clone()
    }

    fn find<'a>(g: &'a [Value], ty: &str) -> Vec<&'a Value> {
        g.iter().filter(|n| n["@type"] == ty).collect()
    }

    #[test]
    fn sections_nest_by_level_and_content_attaches_to_innermost() {
        let els = vec![
            el("doco:SectionTitle", "Chapter", Some(1)),
            el("doco:Paragraph", "In chapter.", None),
            el("doco:SectionTitle", "Part", Some(2)),
            el("doco:Paragraph", "In part.", None),
            el("doco:SectionTitle", "Next chapter", Some(1)),
            el("doco:Paragraph", "After.", None),
        ];
        let g = graph(&els);
        let sections = find(&g, "doco:Section");
        assert_eq!(sections.len(), 3);
        // Chapter contains its title, its paragraph, and the nested Part.
        let chapter = &sections[0];
        let kids = chapter["po:contains"].as_array().unwrap();
        assert_eq!(kids.len(), 3);
        // The level-2 section is inside the level-1, not the body.
        let part_id = sections[1]["@id"].as_str().unwrap();
        assert!(kids.iter().any(|k| k == part_id));
        // "Next chapter" closed both and attached to the body.
        let body = &find(&g, "doco:BodyMatter")[0];
        let body_kids = body["po:contains"].as_array().unwrap();
        let next_id = sections[2]["@id"].as_str().unwrap();
        assert!(body_kids.iter().any(|k| k == next_id));
        assert_eq!(body_kids.len(), 2, "chapter + next chapter");
    }

    #[test]
    fn offsets_index_into_the_text_projection() {
        let els = vec![
            el("doco:SectionTitle", "Title", Some(1)),
            el("doco:Paragraph", "Body text here.", None),
        ];
        let projection = to_text(&els);
        let g = graph(&els);
        for n in g.iter().filter(|n| n.get("nif:beginIndex").is_some()) {
            let s = n["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = n["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            assert_eq!(&slice, n["nif:isString"].as_str().unwrap());
        }
    }

    #[test]
    fn consecutive_list_items_group_under_one_list() {
        let els = vec![
            el("doco:ListItem", "one", None),
            el("doco:ListItem", "two", None),
            el("doco:Paragraph", "break", None),
            el("doco:ListItem", "three", None),
        ];
        let g = graph(&els);
        let lists = find(&g, "doco:List");
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0]["po:contains"].as_array().unwrap().len(), 2);
        assert_eq!(lists[1]["po:contains"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn table_cells_carry_headers_indices_and_values() {
        let mut t = el("doco:Table", String::new().as_str(), None);
        t.cells = Some(vec![
            vec!["".into(), "2023".into(), "2024".into()],
            vec!["Revenue".into(), "10".into(), "".into()],
        ]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        // "Revenue" (no column header), "10" (both headers); empty cell dropped.
        assert_eq!(cells.len(), 2);
        let ten = cells.iter().find(|c| c["doc:cellValue"] == "10").unwrap();
        assert_eq!(ten["doc:rowIndex"], json!(0));
        assert_eq!(ten["doc:columnIndex"], json!(1));
        assert_eq!(ten["doc:columnHeader"], "2023");
        assert_eq!(ten["doc:rowHeader"], "Revenue");
        // The table's projection text is row-major.
        let table = &find(&g, "doco:Table")[0];
        assert!(table["nif:isString"]
            .as_str()
            .unwrap()
            .contains("Revenue\t10"));
    }

    #[test]
    fn table_cells_carry_offsets_into_the_projection() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["".into(), "2023".into(), "2024".into()],
            vec!["Revenue".into(), "10".into(), "".into()],
            vec!["Cost".into(), "7".into(), "8".into()],
        ]);
        // A paragraph before the table so cell offsets must include the
        // table's own start in the document projection, not just the
        // in-table position.
        let els = vec![el("doco:Paragraph", "Intro paragraph.", None), t];
        let projection = to_text(&els);
        let g = graph(&els);
        let cells = find(&g, "doc:TableCell");
        assert_eq!(cells.len(), 5, "five non-empty data cells");
        for cell in &cells {
            let s = cell["nif:beginIndex"].as_u64().unwrap() as usize;
            let e = cell["nif:endIndex"].as_u64().unwrap() as usize;
            let slice: String = projection.chars().skip(s).take(e - s).collect();
            assert_eq!(&slice, cell["doc:cellValue"].as_str().unwrap());
            assert_eq!(&slice, cell["nif:isString"].as_str().unwrap());
        }
        // Same-row cells sit on one projection line: no newline between
        // the row header's end and its neighbour's start.
        let rev = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "Revenue")
            .unwrap();
        let ten = cells.iter().find(|c| c["doc:cellValue"] == "10").unwrap();
        let between: String = {
            let from = rev["nif:endIndex"].as_u64().unwrap() as usize;
            let to = ten["nif:beginIndex"].as_u64().unwrap() as usize;
            projection.chars().skip(from).take(to - from).collect()
        };
        assert_eq!(between, "\t");
    }

    #[test]
    fn filled_down_cells_carry_no_offsets() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["H1".into(), "H2".into()],
            vec!["Alpha".into(), "1".into()],
            vec!["".into(), "2".into()],
        ]);
        // Row 2's first cell continues Alpha from the row above.
        t.merged_down = Some(vec![false, false, false, false, true, false]);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        let filled = cells
            .iter()
            .find(|c| c["doc:rowIndex"] == json!(1) && c["doc:columnIndex"] == json!(0))
            .expect("filled-down cell");
        assert_eq!(filled["doc:cellValue"], "Alpha");
        assert!(
            filled.get("nif:beginIndex").is_none(),
            "a synthesised value has no place in the projection"
        );
        // Its projected neighbour still carries offsets.
        let two = cells.iter().find(|c| c["doc:cellValue"] == "2").unwrap();
        assert!(two.get("nif:beginIndex").is_some());
    }

    #[test]
    fn headerless_table_emits_all_rows_as_data_without_column_headers() {
        let mut t = el("doco:Table", "", None);
        t.cells = Some(vec![
            vec!["".into(), "6,785".into()],
            vec!["Gold".into(), "11,037".into()],
        ]);
        t.header_rows = Some(0);
        let g = graph(&[t]);
        let cells = find(&g, "doc:TableCell");
        // Both non-empty cells of row 0 and row 1 materialise as data.
        assert_eq!(cells.len(), 3);
        let first = cells
            .iter()
            .find(|c| c["doc:cellValue"] == "6,785")
            .unwrap();
        assert_eq!(first["doc:rowIndex"], json!(0));
        assert!(first.get("doc:columnHeader").is_none());
    }

    #[test]
    fn every_element_is_stamped_with_source_doc_page_and_bbox() {
        let g = graph(&[el("doco:Paragraph", "x", None)]);
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["doc:sourceDocument"]["@id"], "urn:test:doc");
        assert_eq!(p["doc:pageIndex"], json!(0));
        assert_eq!(p["doc:bbox"], "10.00,20.00,110.00,40.00");
        // Structural wrappers are stamped too (retract-on-rerun must catch them).
        let doc = &find(&g, "doco:Document")[0];
        assert_eq!(doc["doc:sourceDocument"]["@id"], "urn:test:doc");
    }

    #[test]
    fn label_truncates_but_is_string_survives() {
        let long = "x".repeat(150);
        let g = graph(&[el("doco:Paragraph", &long, None)]);
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["nif:isString"].as_str().unwrap().chars().count(), 150);
        assert_eq!(p["rdfs:label"].as_str().unwrap().chars().count(), 101);
    }

    #[test]
    fn a_link_becomes_a_node_the_element_references() {
        let mut e = el("doco:Paragraph", "see the filing now", None);
        e.links = Some(vec![
            crate::element::Link::uri("https://sec.example/x").spanning(4, 14)
        ]);
        let g = graph(&[e]);
        let link = find(&g, "doc:Link");
        assert_eq!(link.len(), 1);
        assert_eq!(link[0]["doc:linkTarget"]["@id"], "https://sec.example/x");
        assert_eq!(link[0]["nif:isString"], "the filing");

        // The anchor's offsets index the text projection, exactly as an
        // element's do.
        let para = find(&g, "doco:Paragraph");
        let s = link[0]["nif:beginIndex"].as_u64().unwrap() as usize;
        let t = link[0]["nif:endIndex"].as_u64().unwrap() as usize;
        assert_eq!(s, para[0]["nif:beginIndex"].as_u64().unwrap() as usize + 4);
        assert_eq!(t - s, 10);

        let id = link[0]["@id"].as_str().unwrap();
        assert_eq!(para[0]["doc:link"][0], id);
    }

    #[test]
    fn a_turn_carries_its_speaker_and_times_and_its_text_carries_no_time() {
        let mut e = el("doco:Paragraph", "Ada Park: Renewal is at risk.", None);
        e.bbox = None;
        e.turn = Some(crate::element::Turn {
            speaker: Some("Ada Park".into()),
            start_ms: 272_404,
            end_ms: 279_000,
        });
        let g = graph(std::slice::from_ref(&e));
        let p = &find(&g, "doco:Paragraph")[0];
        assert_eq!(p["doc:speaker"], "Ada Park");
        assert_eq!(p["doc:startMs"], json!(272_404));
        assert_eq!(p["doc:endMs"], json!(279_000));
        assert!(p.get("doc:bbox").is_none());
        assert_eq!(to_text(&[e]), "Ada Park: Renewal is at risk.\n");
    }

    #[test]
    fn an_internal_jump_carries_a_page_rather_than_an_address() {
        let mut e = el("doco:Paragraph", "see Chapter 4", None);
        e.links = Some(vec![crate::element::Link::page(11).spanning(4, 13)]);
        let g = graph(&[e]);
        let link = find(&g, "doc:Link");
        assert_eq!(link[0]["doc:linkPage"], 11);
        assert!(link[0].get("doc:linkTarget").is_none());
    }

    fn document_of(elements: &[Element], o: &DocoOptions) -> Value {
        let v: Value = serde_json::from_str(&to_doco(elements, o)).unwrap();
        v["@graph"][0].clone()
    }

    #[test]
    fn an_element_read_from_a_field_says_which() {
        let mut e = el("doco:Paragraph", "Eight programs end.", None);
        e.source_path = Some("/record/lead".into());
        let g = graph(&[e, el("doco:Paragraph", "No field.", None)]);
        let ps = find(&g, "doco:Paragraph");
        assert_eq!(ps[0]["doc:sourcePath"], "/record/lead");
        assert!(ps[1].get("doc:sourcePath").is_none());
    }

    #[test]
    fn a_declared_class_stands_beside_the_document_class() {
        let mut o = opts();
        o.info.class = Some("https://example.org/model#Article".into());
        let doc = document_of(&[], &o);
        assert_eq!(
            doc["@type"],
            json!(["doco:Document", "https://example.org/model#Article"])
        );
        // Undeclared, the type stays the plain string consumers match on.
        assert_eq!(document_of(&[], &opts())["@type"], "doco:Document");
    }

    #[test]
    fn declared_properties_are_statements_of_the_document() {
        let lit = |property: &str, value: &str, datatype: Option<&str>| Property {
            property: property.into(),
            value: PropertyValue::Literal {
                value: value.into(),
                datatype: datatype.map(Into::into),
            },
        };
        let mut o = opts();
        o.info.properties = vec![
            lit("https://example.org/model#recordId", "48213", None),
            lit(
                "https://example.org/model#published",
                "2025-04-12T06:26:14.821Z",
                Some("http://www.w3.org/2001/XMLSchema#dateTime"),
            ),
            Property {
                property: "https://example.org/model#theme".into(),
                value: PropertyValue::Iri {
                    iri: "https://example.org/id/economy".into(),
                },
            },
            lit("https://example.org/model#theme", "Unlisted theme", None),
            Property {
                property: "https://example.org/model#theme".into(),
                value: PropertyValue::Iri {
                    iri: "https://example.org/id/economy".into(),
                },
            },
        ];
        let doc = document_of(&[], &o);
        assert_eq!(doc["https://example.org/model#recordId"], "48213");
        assert_eq!(
            doc["https://example.org/model#published"],
            json!({
                "@value": "2025-04-12T06:26:14.821Z",
                "@type": "http://www.w3.org/2001/XMLSchema#dateTime",
            })
        );
        // Stated twice, two values, in the order stated; a concept is a node
        // and a value the list does not hold stays what was written. The
        // same value stated again, by another field, is stated once.
        assert_eq!(
            doc["https://example.org/model#theme"],
            json!([{ "@id": "https://example.org/id/economy" }, "Unlisted theme"])
        );
    }

    #[test]
    fn a_typed_number_is_written_as_a_number() {
        const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
        let lit = |property: &str, value: &str, datatype: &str| Property {
            property: format!("https://example.org/model#{property}"),
            value: PropertyValue::Literal {
                value: value.into(),
                datatype: Some(format!("{XSD}{datatype}")),
            },
        };
        let mut o = opts();
        o.info.properties = vec![
            lit("duration", "266000", "int"),
            lit("rate", "29.97", "decimal"),
            lit("aired", "true", "boolean"),
            lit("day", "2026-04-24", "date"),
            // Claims a type it is not of: stays the text it is.
            lit("count", "many", "integer"),
        ];
        let doc = document_of(&[], &o);
        let value =
            |property: &str| doc[format!("https://example.org/model#{property}")]["@value"].clone();
        assert_eq!(value("duration"), json!(266000));
        assert_eq!(value("rate"), json!(29.97));
        assert_eq!(value("aired"), json!(true));
        assert_eq!(value("day"), json!("2026-04-24"));
        assert_eq!(value("count"), json!("many"));
    }

    #[test]
    fn a_property_cannot_overwrite_what_the_emitter_states() {
        let mut o = opts();
        o.info.title = Some("The title".into());
        o.info.properties = vec![Property {
            property: "dcterms:title".into(),
            value: PropertyValue::Literal {
                value: "Another".into(),
                datatype: None,
            },
        }];
        assert_eq!(document_of(&[], &o)["dcterms:title"], "The title");
    }

    #[test]
    fn the_source_record_is_kept_in_its_own_order() {
        use crate::element::FieldRole;
        let mut theme = SourceField::new("/record/theme", FieldRole::Enum, "Economy");
        theme.property = Some("https://example.org/model#theme".into());
        theme.iri = Some("https://example.org/id/economy".into());
        let mut o = opts();
        o.fields = vec![
            SourceField::new("/record/id", FieldRole::Unmapped, "48213"),
            SourceField::content("/record/body"),
            theme,
        ];
        let doc = document_of(&[], &o);
        assert_eq!(doc["doc:sourceFields"]["@type"], "@json");
        assert_eq!(
            doc["doc:sourceFields"]["@value"],
            json!([
                { "path": "/record/id", "role": "unmapped", "value": "48213" },
                { "path": "/record/body", "role": "content" },
                {
                    "path": "/record/theme",
                    "role": "enum",
                    "value": "Economy",
                    "property": "https://example.org/model#theme",
                    "iri": "https://example.org/id/economy",
                },
            ])
        );
        assert!(document_of(&[], &opts()).get("doc:sourceFields").is_none());
    }

    #[test]
    fn a_field_that_is_a_payload_is_cut_and_says_so() {
        use crate::element::FieldRole;
        let long = "x".repeat(SourceField::VALUE_MAX_CHARS + 1);
        let f = SourceField::new("Meta:RAWDATA", FieldRole::Unmapped, &long);
        assert!(f.truncated);
        assert_eq!(
            f.value.unwrap().chars().count(),
            SourceField::VALUE_MAX_CHARS
        );
        assert!(!SourceField::new("Meta:TITLE", FieldRole::Unmapped, "short").truncated);
    }
}
