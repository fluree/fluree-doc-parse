//! Outlook's `.msg`: an OLE compound file that holds a message as its MAPI
//! properties, one stream each, with a storage per recipient and per
//! attachment ([MS-OXMSG]).
//!
//! A message Outlook received keeps its internet header in the transport
//! headers property, which is the most complete account of who sent it to
//! whom, when, and in reply to what. One it sent or drafted has none, and
//! is read from the properties instead.

use crate::mime::{self, Headers};
use crate::{address, date, eml, markup, AttachedFile, Body, EmailError, Read};
use cfb::CompoundFile;
use encoding_rs::Encoding;
use fluree_doc_model::{Mailbox, Message};
use std::collections::HashMap;
use std::io::{Cursor, Read as _};

const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// An embedded message nests a whole message inside an attachment; a bound
/// on how deep that goes keeps a hostile file finite.
const MAX_DEPTH: usize = 8;

type Cf<'a> = CompoundFile<Cursor<&'a [u8]>>;

pub fn is_compound(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC)
}

/// A compound file with a message's property stream at its root.
pub fn is_msg(bytes: &[u8]) -> bool {
    is_compound(bytes)
        && CompoundFile::open(Cursor::new(bytes)).is_ok_and(|cf| {
            cf.is_stream("/__properties_version1.0")
                && cf
                    .read_root_storage()
                    .any(|e| e.name().starts_with("__substg1.0_"))
        })
}

pub fn read(bytes: &[u8]) -> Result<Read, EmailError> {
    let mut cf =
        CompoundFile::open(Cursor::new(bytes)).map_err(|e| EmailError::Msg(e.to_string()))?;
    if !cf.is_stream("/__properties_version1.0") {
        return Err(EmailError::Msg("no message properties".into()));
    }
    let m = message(&mut cf, "", 32, 0);
    Ok(Read {
        own: m.own,
        header: m.header,
        body: m.body,
        attachments: m.attachments,
        provenance: "msg",
    })
}

struct Msg {
    own: Message,
    header: String,
    body: Body,
    attachments: Vec<AttachedFile>,
}

// Property identifiers, from [MS-OXPROPS].
const SUBJECT: u16 = 0x0037;
const SENT_REPRESENTING_NAME: u16 = 0x0042;
const TRANSPORT_HEADERS: u16 = 0x007D;
const SENDER_ADDR_TYPE: u16 = 0x0C1E;
const SENDER_EMAIL: u16 = 0x0C1F;
const SENDER_NAME: u16 = 0x0C1A;
const RECIPIENT_TYPE: u16 = 0x0C15;
const DISPLAY_BCC: u16 = 0x0E02;
const DISPLAY_CC: u16 = 0x0E03;
const DISPLAY_TO: u16 = 0x0E04;
const BODY: u16 = 0x1000;
const HTML: u16 = 0x1013;
const INTERNET_MESSAGE_ID: u16 = 0x1035;
const INTERNET_REFERENCES: u16 = 0x1039;
const IN_REPLY_TO: u16 = 0x1042;
const DISPLAY_NAME: u16 = 0x3001;
const ADDR_TYPE: u16 = 0x3002;
const EMAIL_ADDRESS: u16 = 0x3003;
const ATTACH_DATA: u16 = 0x3701;
const ATTACH_FILENAME: u16 = 0x3704;
const ATTACH_METHOD: u16 = 0x3705;
const ATTACH_LONG_FILENAME: u16 = 0x3707;
const ATTACH_MIME_TAG: u16 = 0x370E;
const ATTACH_CONTENT_ID: u16 = 0x3712;
const SMTP_ADDRESS: u16 = 0x39FE;
const SENDER_SMTP: u16 = 0x5D01;
const SENT_REPRESENTING_SMTP: u16 = 0x5D02;
const CLIENT_SUBMIT_TIME: u16 = 0x0039;
const DELIVERY_TIME: u16 = 0x0E06;
const CREATION_TIME: u16 = 0x3007;
const INTERNET_CPID: u16 = 0x3FDE;
const MESSAGE_CODEPAGE: u16 = 0x3FFD;
const ATTACHMENT_HIDDEN: u16 = 0x7FFE;
/// `ATTACH_METHOD` for a message attached whole.
const ATTACH_EMBEDDED_MSG: u32 = 5;

/// A storage's streams and fixed-size properties, read on demand.
struct Store<'s> {
    dir: &'s str,
    /// Fixed-size properties by id: the 8 value bytes.
    fixed: HashMap<u16, [u8; 8]>,
    /// For 8-bit strings, which predate Unicode in the format.
    codepage: &'static Encoding,
}

impl<'s> Store<'s> {
    /// `header_len` is the property stream's header: 32 bytes at the root,
    /// 24 in an embedded message, 8 for a recipient or an attachment.
    fn open(
        cf: &mut Cf,
        dir: &'s str,
        header_len: usize,
        codepage: Option<&'static Encoding>,
    ) -> Self {
        let mut fixed = HashMap::new();
        if let Some(b) = stream(cf, &format!("{dir}/__properties_version1.0")) {
            for rec in b.get(header_len..).unwrap_or(&[]).chunks_exact(16) {
                let tag = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
                let mut value = [0u8; 8];
                value.copy_from_slice(&rec[8..16]);
                fixed.insert((tag >> 16) as u16, value);
            }
        }
        let own = [MESSAGE_CODEPAGE, INTERNET_CPID]
            .iter()
            .find_map(|id| fixed.get(id))
            .map(|v| encoding(u32::from_le_bytes([v[0], v[1], v[2], v[3]])));
        Store {
            dir,
            fixed,
            codepage: own.or(codepage).unwrap_or(encoding_rs::WINDOWS_1252),
        }
    }

    /// A string property, from its UTF-16 stream or its 8-bit one.
    fn string(&self, cf: &mut Cf, id: u16) -> Option<String> {
        let s = match stream(cf, &format!("{}/__substg1.0_{id:04X}001F", self.dir)) {
            Some(b) => utf16le(&b),
            None => {
                let b = stream(cf, &format!("{}/__substg1.0_{id:04X}001E", self.dir))?;
                self.codepage.decode_without_bom_handling(&b).0.into_owned()
            }
        };
        let s = s.trim_end_matches('\0').trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    fn binary(&self, cf: &mut Cf, id: u16) -> Option<Vec<u8>> {
        stream(cf, &format!("{}/__substg1.0_{id:04X}0102", self.dir))
    }

    fn long(&self, id: u16) -> Option<u32> {
        self.fixed
            .get(&id)
            .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }

    fn time(&self, id: u16) -> Option<String> {
        self.fixed
            .get(&id)
            .and_then(|v| date::filetime(u64::from_le_bytes(*v)))
    }

    /// The storages directly under this one whose names start `prefix`.
    fn children(&self, cf: &Cf, prefix: &str) -> Vec<String> {
        let dir = if self.dir.is_empty() { "/" } else { self.dir };
        let mut out: Vec<String> = cf
            .read_storage(dir)
            .map(|entries| {
                entries
                    .filter(|e| e.is_storage() && e.name().starts_with(prefix))
                    .map(|e| e.path().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

fn stream(cf: &mut Cf, path: &str) -> Option<Vec<u8>> {
    let mut s = cf.open_stream(path).ok()?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).ok()?;
    Some(out)
}

fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A Windows code page's encoding, for the ones mail is written in.
fn encoding(cp: u32) -> &'static Encoding {
    use encoding_rs::*;
    match cp {
        65001 | 20127 => UTF_8,
        1200 => UTF_16LE,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 | 28599 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        20866 => KOI8_R,
        21866 => KOI8_U,
        28592 => ISO_8859_2,
        28593 => ISO_8859_3,
        28594 => ISO_8859_4,
        28595 => ISO_8859_5,
        28596 => ISO_8859_6,
        28597 => ISO_8859_7,
        28598 => ISO_8859_8,
        28603 => ISO_8859_13,
        28605 => ISO_8859_15,
        50220..=50222 => ISO_2022_JP,
        51932 => EUC_JP,
        54936 => GB18030,
        _ => WINDOWS_1252,
    }
}

fn message(cf: &mut Cf, dir: &str, header_len: usize, depth: usize) -> Msg {
    let props = Store::open(cf, dir, header_len, None);
    let transport = props
        .string(cf, TRANSPORT_HEADERS)
        .map(|t| Headers::parse(&t));
    let mut own = transport
        .as_ref()
        .filter(|h| h.len() > 0)
        .map(eml::message)
        .unwrap_or_default();

    if own.subject.is_none() {
        own.subject = props.string(cf, SUBJECT);
    }
    if own.from.is_empty() {
        let name = props
            .string(cf, SENDER_NAME)
            .or_else(|| props.string(cf, SENT_REPRESENTING_NAME));
        let address = sender_address(cf, &props);
        if name.is_some() || address.is_some() {
            own.from = vec![Mailbox { name, address }];
        }
    }
    if own.to.is_empty() && own.cc.is_empty() && own.bcc.is_empty() {
        recipients(cf, &props, &mut own);
    }
    if own.date.is_none() {
        own.date = [CLIENT_SUBMIT_TIME, DELIVERY_TIME, CREATION_TIME]
            .iter()
            .find_map(|&id| props.time(id));
    }
    if own.message_id.is_none() {
        own.message_id = props
            .string(cf, INTERNET_MESSAGE_ID)
            .map(|s| s.trim_matches(['<', '>']).to_string());
    }
    for (id, slot) in [
        (IN_REPLY_TO, &mut own.in_reply_to),
        (INTERNET_REFERENCES, &mut own.references),
    ] {
        if slot.is_empty() {
            if let Some(v) = props.string(cf, id) {
                *slot = v
                    .split_whitespace()
                    .map(|s| s.trim_matches(['<', '>']).to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }

    let raw_date = transport
        .as_ref()
        .and_then(|h| h.get("date"))
        .map(str::to_string);
    let shown = raw_date.or_else(|| own.date.as_deref().and_then(date::to_rfc5322));
    let header = crate::header_text(&own, shown.as_deref());

    // The plain text, unless a sender's converter left markup in it.
    let body = match (
        props.string(cf, BODY).map(|b| b.replace("\r\n", "\n")),
        html(cf, &props),
    ) {
        (Some(p), Some(h)) if markup::unrendered(&p, &h) => Body::Html(h),
        (Some(p), _) => Body::Plain(p),
        (None, Some(h)) => Body::Html(h),
        (None, None) => Body::None,
    };
    let attachments = attachments(cf, &props, depth);
    Msg {
        own,
        header,
        body,
        attachments,
    }
}

/// The sender's internet address. Exchange addresses a sender inside the
/// organisation by directory path (`/O=EXCHANGELABS/OU=…`), which is not an
/// address anyone can write to; the SMTP property holds the one that is.
fn sender_address(cf: &mut Cf, props: &Store) -> Option<String> {
    props
        .string(cf, SENDER_SMTP)
        .or_else(|| props.string(cf, SENT_REPRESENTING_SMTP))
        .or_else(|| {
            let smtp = props
                .string(cf, SENDER_ADDR_TYPE)
                .is_some_and(|t| t.eq_ignore_ascii_case("SMTP"));
            smtp.then(|| props.string(cf, SENDER_EMAIL)).flatten()
        })
}

fn recipients(cf: &mut Cf, props: &Store, own: &mut Message) {
    for dir in props.children(cf, "__recip_version1.0_") {
        let r = Store::open(cf, &dir, 8, Some(props.codepage));
        let name = r.string(cf, DISPLAY_NAME);
        let address = r.string(cf, SMTP_ADDRESS).or_else(|| {
            let smtp = r
                .string(cf, ADDR_TYPE)
                .is_some_and(|t| t.eq_ignore_ascii_case("SMTP"));
            smtp.then(|| r.string(cf, EMAIL_ADDRESS)).flatten()
        });
        if name.is_none() && address.is_none() {
            continue;
        }
        let mailbox = Mailbox { name, address };
        match r.long(RECIPIENT_TYPE) {
            Some(2) => own.cc.push(mailbox),
            Some(3) => own.bcc.push(mailbox),
            _ => own.to.push(mailbox),
        }
    }
    // A message without recipient storages still lists their names.
    if own.to.is_empty() && own.cc.is_empty() && own.bcc.is_empty() {
        for (id, slot) in [
            (DISPLAY_TO, &mut own.to),
            (DISPLAY_CC, &mut own.cc),
            (DISPLAY_BCC, &mut own.bcc),
        ] {
            if let Some(v) = props.string(cf, id) {
                *slot = address::loose(&v);
            }
        }
    }
}

/// The HTML body, in the code page the message declares for its internet
/// form.
fn html(cf: &mut Cf, props: &Store) -> Option<String> {
    if let Some(b) = props.binary(cf, HTML) {
        let cp = props.long(INTERNET_CPID).map(encoding);
        let text = match cp {
            Some(e) => e.decode(&b).0.into_owned(),
            None => mime::decode_charset(&b, None),
        };
        return Some(text).filter(|t| !t.trim().is_empty());
    }
    props.string(cf, HTML)
}

fn attachments(cf: &mut Cf, props: &Store, depth: usize) -> Vec<AttachedFile> {
    let mut out = Vec::new();
    for dir in props.children(cf, "__attach_version1.0_") {
        let a = Store::open(cf, &dir, 8, Some(props.codepage));
        let filename = [ATTACH_LONG_FILENAME, ATTACH_FILENAME, DISPLAY_NAME]
            .iter()
            .find_map(|&id| a.string(cf, id))
            .and_then(|n| crate::base_name(&n));
        let embedded = format!("{dir}/__substg1.0_{ATTACH_DATA:04X}000D");
        if a.long(ATTACH_METHOD) == Some(ATTACH_EMBEDDED_MSG) || cf.is_storage(&embedded) {
            if depth >= MAX_DEPTH || !cf.is_storage(&embedded) {
                continue;
            }
            // A message attached whole comes back as an `.eml` of its own,
            // so it reaches its reader as any forwarded message does.
            let inner = message(cf, &embedded, 24, depth + 1);
            let bytes = write_eml(&inner);
            let stem = inner
                .own
                .subject
                .clone()
                .or_else(|| {
                    filename
                        .as_deref()
                        .map(|f| f.trim_end_matches(".msg").to_string())
                })
                .unwrap_or_else(|| "message".into());
            out.push(AttachedFile::new(
                Some(format!("{stem}.eml")),
                "message/rfc822".into(),
                false,
                bytes,
            ));
            continue;
        }
        let Some(bytes) = a.binary(cf, ATTACH_DATA) else {
            continue;
        };
        let content_type = a
            .string(cf, ATTACH_MIME_TAG)
            .map(|t| t.to_ascii_lowercase())
            .unwrap_or_else(|| guess_type(filename.as_deref()).to_string());
        let hidden = a.fixed.get(&ATTACHMENT_HIDDEN).is_some_and(|v| v[0] != 0);
        let inline = hidden
            || (content_type.starts_with("image/") && a.string(cf, ATTACH_CONTENT_ID).is_some());
        out.push(AttachedFile::new(filename, content_type, inline, bytes));
    }
    out
}

/// A type for an attachment that does not declare one, by its extension.
fn guess_type(filename: Option<&str>) -> &'static str {
    let ext = filename
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "htm" | "html" => "text/html",
        "md" => "text/markdown",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "tif" | "tiff" => "image/tiff",
        "zip" => "application/zip",
        "ics" => "text/calendar",
        _ => "application/octet-stream",
    }
}

/// An embedded message as RFC 5322 bytes: its header, its body, and its own
/// attachments base64-encoded in a `multipart/mixed`.
fn write_eml(m: &Msg) -> Vec<u8> {
    let list = |b: &[Mailbox]| {
        b.iter()
            .map(Mailbox::display)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = String::new();
    for (name, boxes) in [("From", &m.own.from), ("To", &m.own.to), ("Cc", &m.own.cc)] {
        if !boxes.is_empty() {
            out.push_str(&format!("{name}: {}\r\n", list(boxes)));
        }
    }
    if let Some(d) = m.own.date.as_deref().and_then(date::to_rfc5322) {
        out.push_str(&format!("Date: {d}\r\n"));
    }
    if let Some(s) = &m.own.subject {
        out.push_str(&format!("Subject: {s}\r\n"));
    }
    if let Some(id) = &m.own.message_id {
        out.push_str(&format!("Message-ID: <{id}>\r\n"));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    let (kind, text) = match &m.body {
        Body::Plain(t) => ("text/plain", t.as_str()),
        Body::Html(h) => ("text/html", h.as_str()),
        Body::None => ("text/plain", ""),
    };
    let body_part = format!(
        "Content-Type: {kind}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
        base64_lines(text.as_bytes())
    );
    if m.attachments.is_empty() {
        out.push_str(&body_part);
        return out.into_bytes();
    }
    let boundary = "=_fluree-doc-email-embedded";
    out.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n{body_part}"
    ));
    for a in &m.attachments {
        let name = a
            .info
            .filename
            .as_deref()
            .unwrap_or("attachment")
            .replace('"', "'");
        out.push_str(&format!(
            "--{boundary}\r\nContent-Type: {}\r\nContent-Disposition: {}; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            a.info.content_type,
            if a.info.inline { "inline" } else { "attachment" },
            base64_lines(&a.bytes)
        ));
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    out.into_bytes()
}

fn base64_lines(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + bytes.len() / 57 * 2 + 4);
    for (i, chunk) in bytes.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push_str("\r\n");
        }
        let n =
            chunk.iter().fold(0u32, |acc, &b| acc << 8 | u32::from(b)) << (8 * (3 - chunk.len()));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write as _;

    /// A property stream entry: tag (type | id << 16), flags, 8 value bytes.
    fn prop(id: u16, ty: u16, value: u64) -> Vec<u8> {
        let mut v = (u32::from(id) << 16 | u32::from(ty)).to_le_bytes().to_vec();
        v.extend(6u32.to_le_bytes());
        v.extend(value.to_le_bytes());
        v
    }

    fn put(cf: &mut CompoundFile<Cursor<Vec<u8>>>, path: &str, bytes: &[u8]) {
        cf.create_stream(path).unwrap().write_all(bytes).unwrap();
    }

    fn unicode(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A small Outlook message built the way Outlook lays one out: a sent
    /// item with no transport headers, two recipients, a PDF and a message
    /// attached whole.
    pub fn sample() -> Vec<u8> {
        let mut cf = CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        let mut root = vec![0u8; 32];
        root.extend(prop(CLIENT_SUBMIT_TIME, 0x0040, 134_287_876_800_000_000));
        put(&mut cf, "/__properties_version1.0", &root);
        put(&mut cf, "/__substg1.0_0037001F", &unicode("RE: Budget"));
        put(&mut cf, "/__substg1.0_0C1A001F", &unicode("Lena Holt"));
        put(
            &mut cf,
            "/__substg1.0_5D01001F",
            &unicode("lena@example.com"),
        );
        put(
            &mut cf,
            "/__substg1.0_1000001F",
            &unicode("Approved.\r\n\r\nFrom: Kai Moreno <kai@example.com>\r\nSent: Thursday, July 16, 2026 8:42 AM\r\nSubject: Budget\r\n\r\nCan you approve it?\r\n"),
        );
        for (i, (name, addr, kind)) in [
            ("Kai Moreno", "kai@example.com", 1u64),
            ("Ops Team", "ops@example.com", 2),
        ]
        .iter()
        .enumerate()
        {
            let dir = format!("/__recip_version1.0_#{i:08X}");
            cf.create_storage(&dir).unwrap();
            let mut p = vec![0u8; 8];
            p.extend(prop(RECIPIENT_TYPE, 0x0003, *kind));
            put(&mut cf, &format!("{dir}/__properties_version1.0"), &p);
            put(
                &mut cf,
                &format!("{dir}/__substg1.0_3001001F"),
                &unicode(name),
            );
            put(
                &mut cf,
                &format!("{dir}/__substg1.0_39FE001F"),
                &unicode(addr),
            );
        }
        let a0 = "/__attach_version1.0_#00000000";
        cf.create_storage(a0).unwrap();
        let mut p = vec![0u8; 8];
        p.extend(prop(ATTACH_METHOD, 0x0003, 1));
        put(&mut cf, &format!("{a0}/__properties_version1.0"), &p);
        put(
            &mut cf,
            &format!("{a0}/__substg1.0_3707001F"),
            &unicode("budget.pdf"),
        );
        put(
            &mut cf,
            &format!("{a0}/__substg1.0_37010102"),
            b"%PDF-1.7 stand-in",
        );

        let a1 = "/__attach_version1.0_#00000001";
        cf.create_storage(a1).unwrap();
        let mut p = vec![0u8; 8];
        p.extend(prop(ATTACH_METHOD, 0x0003, u64::from(ATTACH_EMBEDDED_MSG)));
        put(&mut cf, &format!("{a1}/__properties_version1.0"), &p);
        let inner = format!("{a1}/__substg1.0_3701000D");
        cf.create_storage(&inner).unwrap();
        put(
            &mut cf,
            &format!("{inner}/__properties_version1.0"),
            &[0u8; 24],
        );
        put(
            &mut cf,
            &format!("{inner}/__substg1.0_0037001F"),
            &unicode("Kickoff"),
        );
        put(
            &mut cf,
            &format!("{inner}/__substg1.0_0C1A001F"),
            &unicode("Kai Moreno"),
        );
        put(
            &mut cf,
            &format!("{inner}/__substg1.0_1000001F"),
            &unicode("See you Tuesday."),
        );
        cf.flush().unwrap();
        cf.into_inner().into_inner()
    }

    #[test]
    fn an_outlook_message_reads_like_an_email() {
        let bytes = sample();
        assert!(is_msg(&bytes));
        let email = crate::parse(&bytes).unwrap();
        let head = email.elements[0].message.as_deref().unwrap();
        assert_eq!(head.from[0].display(), "Lena Holt <lena@example.com>");
        assert_eq!(head.to[0].address.as_deref(), Some("kai@example.com"));
        assert_eq!(head.cc[0].name.as_deref(), Some("Ops Team"));
        assert_eq!(head.date.as_deref(), Some("2026-07-17T18:48:00Z"));
        assert!(email.elements[0]
            .text
            .contains("Date: Fri, 17 Jul 2026 18:48:00 +0000"));
        assert_eq!(email.elements[1].text, "Approved.");
        // The Outlook header block in the body opens the quoted message.
        let quoted = email.elements[2].message.as_deref().unwrap();
        assert!(quoted.quoted);
        assert_eq!(quoted.from[0].name.as_deref(), Some("Kai Moreno"));
        assert_eq!(email.elements[3].text, "Can you approve it?");
        assert_eq!(email.elements[0].provenance, "msg");

        assert_eq!(email.attachments.len(), 2);
        assert_eq!(
            email.attachments[0].info.filename.as_deref(),
            Some("budget.pdf")
        );
        assert_eq!(email.attachments[0].info.content_type, "application/pdf");
        assert_eq!(email.attachments[0].bytes, b"%PDF-1.7 stand-in");
        // The message attached whole comes back as an email of its own.
        let fwd = &email.attachments[1];
        assert_eq!(fwd.info.filename.as_deref(), Some("Kickoff.eml"));
        let inner = crate::parse(&fwd.bytes).unwrap();
        assert_eq!(inner.info.title.as_deref(), Some("Kickoff"));
        assert_eq!(inner.elements[1].text, "See you Tuesday.");
    }

    #[test]
    fn a_compound_file_that_is_not_a_message_is_refused() {
        let mut cf = CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        put(&mut cf, "/WordDocument", b"not mail");
        cf.flush().unwrap();
        let bytes = cf.into_inner().into_inner();
        assert!(!is_msg(&bytes));
        assert!(matches!(crate::parse(&bytes), Err(EmailError::Msg(_))));
    }

    #[test]
    fn base64_encodes_in_lines() {
        assert_eq!(base64_lines(b"Hello"), "SGVsbG8=");
        assert_eq!(
            mime::base64(base64_lines(&[7u8; 200]).as_bytes()),
            vec![7u8; 200]
        );
    }
}
