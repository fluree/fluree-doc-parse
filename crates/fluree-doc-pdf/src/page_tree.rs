//! The shape of a PDF's page tree, measured before the parser walks it.
//!
//! hayro-syntax walks the page tree when a document is opened
//! (`Pdf::new`), by recursion, one call for each level of `/Pages` nodes,
//! with no bound on the depth: a 1.7 MB file of 20,000 nested `/Pages`
//! nodes overflowed the main thread's 8 MiB stack, and 5,000 a batch
//! worker's 2 MiB, aborting the process whatever the reader then does. It
//! keeps every page it walks, and a node may be named many times: a 12 KB
//! file whose two levels each list one kid a thousand times made a million
//! pages, 1.4 GB. The parser offers no hook into that walk, and a document
//! cannot be had without it, so the tree is measured first from the file's
//! bytes ([`measure`]), and a document nesting deeper than
//! [`MAX_PAGE_TREE_DEPTH`], or listing more than [`MAX_PAGE_TREE_PAGES`],
//! is refused before it is opened ([`open`]).
//!
//! The measure reads every object the file holds as hayro-syntax reads
//! one, where it is written and inside object streams, each version an
//! incremental update left:
//!
//! - an object's header is `N G obj` as the parser's lexer reads it: white
//!   space or comments between its parts, glued to what precedes it
//!   (`endobj3 0 obj`);
//! - an object stream is read at its `/Length`, given by reference or not,
//!   decoded by the parser's own filters (any chain of Flate, LZW,
//!   ASCIIHex, ASCII85 and RunLength, with their parameters), each stage
//!   bounded before it runs, and its objects read in place, each numbered
//!   both as the parser finds it, by its index in the stream, from the
//!   entries of the file's cross-reference streams, and as the parser
//!   numbers it when it rebuilds a broken table, by the number the
//!   stream's header writes;
//! - a `/Pages` node (or one whose `/Type` is a reference, which may
//!   resolve to it) is a level, and its `/Kids` (an array, inline or a
//!   reference to one, of references or inline nodes) are followed to every
//!   version of the objects they name.
//!
//! Over that graph the depth and the pages are no fewer than the parser's
//! walk makes, whichever version its cross-reference table picks: a node
//! named twice is counted twice, and a cycle, which the parser cuts where
//! it meets an object already on its path, is counted as though every path
//! through it were walked. The walk starts at the object a catalog names by
//! `/Pages`, so only what is reachable from one is measured: a structure
//! tree's array of a million references, or a name tree's `/Kids`, are no
//! pages. Where no dictionary naming `/Pages` is seen (a catalog packed in
//! an object stream the measure cannot decode), every node is measured.
//!
//! What the measure does not see is listed in `docs/reference/limits.md`
//! (*A PDF's page tree*); the parser walking the tree with a bound of its
//! own is the complete answer.

use crate::extract::ExtractError;
use fluree_doc_model::budget::{scaled, TooLarge};
use hayro_syntax::bit_reader::BitReader;
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, Stream};
use hayro_syntax::reader::{Reader, ReaderContext, ReaderExt};
use hayro_syntax::Pdf;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;

/// The deepest a page tree may nest: 256 levels. A real tree is a handful
/// deep (a balanced one of a billion pages, a dozen); a degenerate one,
/// each node a page and the next node, is as deep as it has pages, and
/// overflowed the parser's stack from 5,000 on a 2 MiB thread. A release
/// build overflowed 8 MiB between 5,000 and 10,000 levels, about 1 KB a
/// level; 256 levels are walked on a 2 MiB thread in a debug build (the
/// tests' own).
pub const MAX_PAGE_TREE_DEPTH: usize = 256;

/// The most pages a page tree may list: 250,000. The parser keeps each
/// page it walks, about 1.4 KB of them, before the page limit can leave
/// any unread: a tree of 250,000 pages took 360 MB more than one of a
/// single page as `fdoc convert` opened it. This bounds that to about 370
/// MB, within the 1.25 GiB a batch keeps beside each document's budget for
/// what the readers' own bounds let them take ([Batches] in
/// `docs/reference/limits.md`), with [`MAX_XREF_ENTRIES`]'s. The longest
/// real documents run to tens of thousands of pages.
pub const MAX_PAGE_TREE_PAGES: u64 = 250_000;

/// The deepest an object may nest its dictionaries and arrays: 2,048. The
/// parser reads one by recursion, and on a 2 MiB thread overflowed past
/// about 22,000 dictionaries nested in a release build, 5,400 in a debug
/// one (100,000 overflowed the main thread's 8 MiB); a real object nests a
/// handful deep.
pub const MAX_OBJECT_NESTING: usize = 2048;

/// The most bytes one object stream, or cross-reference stream, is
/// decoded to: 64 MiB. A stream that would decode past it refuses the
/// document: the parser would decode it whole, without a bound, were it to
/// read it.
pub const MAX_OBJECT_STREAM_BYTES: u64 = scaled(64 << 20);

/// The most bytes the file's object streams and cross-reference streams
/// are decoded to in all: 256 MiB. A real file's hold its small objects
/// and their offsets, a few megabytes.
pub const MAX_OBJECT_STREAMS_BYTES: u64 = scaled(256 << 20);

/// The most entries the file's cross-reference streams may list in all:
/// four million. The parser keeps each in its table, about 88 bytes with
/// the stream's rows (30 million, from a 219 KB file, took 2.6 GB as `fdoc
/// convert` opened it), so this bounds it to about 350 MB, as
/// [`MAX_PAGE_TREE_PAGES`] bounds its pages. A real file lists one entry
/// for each of its objects, a few hundred thousand for the longest.
pub const MAX_XREF_ENTRIES: u64 = scaled(4_000_000);

/// How many times over the file, and over what its object streams decode
/// to, the measure may scan, reading its objects where the parser could
/// read them: a real file is scanned about once, an object that starts
/// where another one does not end being rare. What object streams decode
/// to counts beside the file: their objects are scanned there, and a
/// stream compresses its objects many times over.
const SCANS: usize = 16;

/// Open `data` as a PDF, its page tree measured first: past
/// [`MAX_PAGE_TREE_DEPTH`] or [`MAX_PAGE_TREE_PAGES`], or where it cannot
/// be measured within the bounds above, it is refused rather than walked
/// into a stack overflow or an allocation that fails.
pub fn open(data: impl Into<Arc<Vec<u8>>>) -> Result<Pdf, ExtractError> {
    let data = data.into();
    let shape = measure(&data).map_err(ExtractError::TooLarge)?;
    if shape.depth > MAX_PAGE_TREE_DEPTH {
        return Err(ExtractError::TooLarge(TooLarge::fixed(format!(
            "its page tree nests more than {MAX_PAGE_TREE_DEPTH} deep"
        ))));
    }
    if shape.pages > MAX_PAGE_TREE_PAGES {
        return Err(ExtractError::TooLarge(TooLarge::fixed(format!(
            "its page tree lists more than {MAX_PAGE_TREE_PAGES} pages"
        ))));
    }
    Pdf::new(data).map_err(|e| ExtractError::Parse(format!("{e:?}")))
}

/// What the measure found: no less than the parser's walk of the tree
/// makes, whichever version of each object it reads, from whichever
/// catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// The most `/Pages` nodes on one path down the tree.
    pub depth: usize,
    /// The pages the walk keeps: each path to a node that is not a
    /// `/Pages` node, saturating.
    pub pages: u64,
}

/// Measure the page tree of the PDF in `data`, over every object the file
/// holds. An error is the file refused before the measure could finish:
/// an object nesting past [`MAX_OBJECT_NESTING`], object streams decoding
/// past their bounds, objects overlapping past [`SCANS`] passes over the
/// file and what its object streams decode to.
pub fn measure(data: &[u8]) -> Result<Shape, TooLarge> {
    let mut m = Measure {
        data,
        graph: Graph::default(),
        last: HashMap::new(),
        packed: HashMap::new(),
        entries: 0,
        scanned: 0,
        decoded: 0,
    };
    let mut streams = Vec::new();
    for (body, numbers) in headers(data) {
        if let Some(stream) = m.top_level(body, &numbers)? {
            streams.push((body, stream, numbers));
        }
    }
    // The cross-reference streams first: the numbers they give the objects
    // of each object stream, by index.
    for (body, stream, _) in &streams {
        if stream.xref {
            m.xref_stream(*body)?;
        }
    }
    for (body, stream, numbers) in &streams {
        if stream.packed {
            m.object_stream(*body, numbers)?;
        }
    }
    Ok(m.graph.shape())
}

/// Each object written in `data`: where its body starts, after `obj`, and
/// every number the parser could read it as. Found from each `obj` the
/// file holds, read back over white space and comments, as the parser's
/// lexer reads forward: a number, a generation, then `obj`.
fn headers(data: &[u8]) -> Vec<(usize, Vec<i32>)> {
    let mut out = Vec::new();
    for at in memchr::memmem::find_iter(data, b"obj") {
        let mut numbers = Vec::new();
        for generation_end in token_ends_before(data, at) {
            let generation = run_start(data, generation_end);
            // A number, then white space or a comment, then the generation.
            if generation == 0 || !is_white(data[generation - 1]) {
                continue;
            }
            if integer(data, generation, generation_end).is_none() {
                continue;
            }
            for number_end in token_ends_before(data, generation) {
                let n = integer(data, run_start(data, number_end), number_end);
                if let Some(n) = n.filter(|n| !numbers.contains(n)) {
                    numbers.push(n);
                }
            }
        }
        if !numbers.is_empty() {
            out.push((at + 3, numbers));
        }
    }
    out
}

/// The positions `e` before `t` where a number may end such that the
/// parser, skipping white space and comments from `e` as it does after a
/// number, stops at `t`: `data[e - 1]` may end a number, and something is
/// skipped (a number followed directly by a letter or a digit is not one).
///
/// Read backward: a position is skipped to `t` when it is white space
/// before one that is, or a `%` whose line ends in one that is. Each
/// position is skipped to one `t` only, so the walks back from every `obj`
/// cover the file about once.
fn token_ends_before(data: &[u8], t: usize) -> Vec<usize> {
    let mut out = Vec::new();
    // Whether skipping from the position after `q` stops at `t`; and from
    // the end of the line `q` is on.
    let mut next = true;
    let mut eol = false;
    let mut q = t;
    while q > 0 {
        q -= 1;
        let c = data[q];
        let here = if is_white(c) {
            next
        } else if c == b'%' {
            eol
        } else {
            false
        };
        if matches!(c, b'\n' | b'\r') {
            eol = here;
        }
        if !here {
            if next && q + 1 < t && is_number_char(c) {
                out.push(q + 1);
            }
            if !eol {
                break;
            }
        }
        next = here;
    }
    out
}

/// Where the run of characters a number may hold, ending at `end`, starts.
fn run_start(data: &[u8], end: usize) -> usize {
    let mut s = end;
    while s > 0 && is_number_char(data[s - 1]) {
        s -= 1;
    }
    s
}

/// The integer the parser reads at `data[start..]`, if it ends at `end`.
fn integer(data: &[u8], start: usize, end: usize) -> Option<i32> {
    let mut r = Reader::new(&data[start..]);
    let n = r.read_without_context::<i32>()?;
    (start + r.offset() == end).then_some(n)
}

fn is_white(c: u8) -> bool {
    matches!(c, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn is_number_char(c: u8) -> bool {
    c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.')
}

fn is_delimiter(c: u8) -> bool {
    matches!(
        c,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(c: u8) -> bool {
    !is_white(c) && !is_delimiter(c)
}

/// How an object written at some offset reads, scanned as the parser
/// skips it: a dictionary or an array, nesting no deeper than
/// [`MAX_OBJECT_NESTING`], and where it ends.
enum Scanned {
    /// A dictionary or an array, ending at this offset.
    Container(usize),
    /// Not one: a name, a number, a string, or nothing readable.
    Other,
    /// A container nesting past [`MAX_OBJECT_NESTING`].
    TooDeep,
}

/// Scan the object at the start of `data` (after white space and
/// comments) as the parser skips one, without recursion: what it is, and
/// for a dictionary or an array, where it ends.
fn scan(data: &[u8]) -> Scanned {
    #[derive(PartialEq)]
    enum Open {
        Dict,
        Array,
    }
    let mut open: Vec<Open> = Vec::new();
    let mut i = 0;
    loop {
        // White space and comments.
        while i < data.len() {
            match data[i] {
                c if is_white(c) => i += 1,
                b'%' => {
                    while i < data.len() && !matches!(data[i], b'\n' | b'\r') {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        let Some(&c) = data.get(i) else {
            return Scanned::Other;
        };
        let pair = data.get(i + 1).copied();
        match (c, pair) {
            (b'<', Some(b'<')) | (b'[', _) => {
                open.push(if c == b'[' { Open::Array } else { Open::Dict });
                if open.len() > MAX_OBJECT_NESTING {
                    return Scanned::TooDeep;
                }
                i += if c == b'[' { 1 } else { 2 };
                continue;
            }
            _ if open.is_empty() => return Scanned::Other,
            (b'>', Some(b'>')) | (b']', _) => {
                let closes = if c == b']' { Open::Array } else { Open::Dict };
                if open.pop() != Some(closes) {
                    return Scanned::Other;
                }
                i += if c == b']' { 1 } else { 2 };
                if open.is_empty() {
                    return Scanned::Container(i);
                }
                continue;
            }
            (b'(', _) => {
                // A literal string: balanced parentheses, a backslash
                // escaping the byte after it.
                let mut depth = 0usize;
                loop {
                    let Some(&b) = data.get(i) else {
                        return Scanned::Other;
                    };
                    i += 1;
                    match b {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            (b'<', _) => {
                // A hex string: hex digits and white space, then `>`.
                i += 1;
                while i < data.len() && (data[i].is_ascii_hexdigit() || is_white(data[i])) {
                    i += 1;
                }
                if data.get(i) != Some(&b'>') {
                    return Scanned::Other;
                }
                i += 1;
            }
            (b'/', _) => {
                i += 1;
                while i < data.len() && is_regular(data[i]) {
                    i += 1;
                }
            }
            (c, _) if is_regular(c) => {
                // A number, a keyword, a reference's parts, or what the
                // parser skips leniently in a container.
                while i < data.len() && is_regular(data[i]) {
                    i += 1;
                }
            }
            _ => return Scanned::Other,
        }
    }
}

/// What a filter decodes, as the parser names them.
#[derive(Clone, Copy, PartialEq)]
enum Filter {
    AsciiHex,
    Ascii85,
    Lzw,
    Flate,
    RunLength,
    /// An image filter or `Crypt`: an object stream coded by one is not
    /// read here.
    Other,
}

impl Filter {
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"ASCIIHexDecode" | b"AHx" => Self::AsciiHex,
            b"ASCII85Decode" | b"A85" => Self::Ascii85,
            b"LZWDecode" | b"LZW" => Self::Lzw,
            b"FlateDecode" | b"Fl" => Self::Flate,
            b"RunLengthDecode" | b"RL" => Self::RunLength,
            b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode" | b"DCTDecode" | b"DCT" | b"JPXDecode"
            | b"Crypt" => Self::Other,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::AsciiHex => "ASCIIHexDecode",
            Self::Ascii85 => "ASCII85Decode",
            Self::Lzw => "LZWDecode",
            Self::Flate => "FlateDecode",
            Self::RunLength => "RunLengthDecode",
            Self::Other => "Crypt",
        }
    }
}

struct Measure<'a> {
    data: &'a [u8],
    graph: Graph,
    /// Where the body of the last object written with each number starts:
    /// what a value given by reference in an object stream's dictionary
    /// resolves to.
    last: HashMap<i32, usize>,
    /// The objects a cross-reference stream places in an object stream:
    /// by the stream's number, each object's number and its index there.
    packed: HashMap<i32, Vec<(i32, u32)>>,
    /// The entries read from cross-reference streams, against
    /// [`MAX_XREF_ENTRIES`].
    entries: u64,
    /// Bytes scanned, against [`SCANS`] passes over the file and
    /// [`Measure::decoded`].
    scanned: usize,
    /// Bytes object streams were decoded to, against
    /// [`MAX_OBJECT_STREAMS_BYTES`].
    decoded: u64,
}

/// What a stream written in the file may be, by its dictionary.
#[derive(Clone, Copy)]
struct StreamKind {
    /// An object stream: `/N` and `/First`.
    packed: bool,
    /// A cross-reference stream: `/W` and `/Size`, which the parser reads
    /// wherever the file's `startxref`, a `/Prev` or an `/XRefStm` points,
    /// whatever its `/Type`.
    xref: bool,
}

impl<'a> Measure<'a> {
    /// Spend `n` bytes of scanning: past [`SCANS`] passes over the file
    /// and what its object streams decoded to, the file is refused. That is
    /// bounded too, by [`MAX_OBJECT_STREAMS_BYTES`], so the scan is.
    fn scan_spent(&mut self, n: usize) -> Result<(), TooLarge> {
        self.scanned = self.scanned.saturating_add(n);
        let read = self
            .data
            .len()
            .saturating_add(usize::try_from(self.decoded).unwrap_or(usize::MAX));
        if self.scanned > SCANS.saturating_mul(read).saturating_add(1 << 20) {
            return Err(TooLarge::fixed(format!(
                "its objects overlap past {SCANS} passes over the file and its object streams"
            )));
        }
        Ok(())
    }

    /// Read the object whose body starts at `body`, numbered `numbers`:
    /// into the graph, and, where it is a stream that may be an object
    /// stream or a cross-reference stream, which, for
    /// [`Measure::object_stream`] and [`Measure::xref_stream`].
    fn top_level(&mut self, body: usize, numbers: &[i32]) -> Result<Option<StreamKind>, TooLarge> {
        for &n in numbers {
            self.last.insert(n, body);
        }
        let data = self.data;
        let Some((object, end)) = self.read(data, body, data.len())? else {
            return Ok(None);
        };
        // A dictionary followed by `stream` is read by the parser as a
        // stream.
        let stream = match &object {
            Object::Dict(d) => {
                let kind = StreamKind {
                    packed: d.contains_key(b"N") && d.contains_key(b"First"),
                    xref: d.contains_key(b"W") && d.contains_key(b"Size"),
                };
                let mut r = Reader::new(&data[end..]);
                r.skip_white_spaces_and_comments();
                (kind.packed || kind.xref)
                    .then(|| r.forward_tag(b"stream"))
                    .flatten()
                    .map(|()| kind)
            }
            _ => None,
        };
        let node = self.graph.add(&object, 0);
        if let Some(id) = node {
            self.graph.nodes[id].cuts = true;
        }
        let dict = matches!(object, Object::Dict(_) | Object::Stream(_));
        for &n in numbers {
            self.graph.number(n, node, dict);
        }
        Ok(stream)
    }

    /// The object at `data[at..end]` as the parser reads it, a dictionary
    /// or an array (a stream's dictionary alone), and where it ends; `None`
    /// for anything else. Scanned first, so that one nesting too deep for
    /// the parser's recursion is refused before it is read.
    fn read<'d>(
        &mut self,
        data: &'d [u8],
        at: usize,
        end: usize,
    ) -> Result<Option<(Object<'d>, usize)>, TooLarge> {
        let slice = &data[at..end];
        let mut r = Reader::new(slice);
        r.skip_white_spaces_and_comments();
        let start = r.offset();
        let scanned = scan(&slice[start..]);
        let len = match scanned {
            Scanned::Container(len) => len,
            Scanned::Other => {
                // A name or a number is read, for a `/Type` or a value an
                // object stream's dictionary gives by reference.
                return Ok(match slice.get(start) {
                    Some(b'/') => r
                        .read_with_context::<Name<'_>>(&ReaderContext::dummy())
                        .map(|n| (Object::Name(n), at + r.offset())),
                    _ => None,
                });
            }
            Scanned::TooDeep => {
                return Err(TooLarge::fixed(format!(
                    "an object nests more than {MAX_OBJECT_NESTING} deep"
                )))
            }
        };
        self.scan_spent(len)?;
        let mut r = Reader::new(&slice[..start + len]);
        r.jump(start);
        Ok(r.read_with_context::<Object<'_>>(&ReaderContext::dummy())
            .map(|o| (o, at + r.offset())))
    }

    /// The dictionary of the stream whose body starts at `body`, and where
    /// it ends. Scanned already, as [`Measure::top_level`] read it.
    fn stream_dict(&self, body: usize) -> Option<(Dict<'a>, usize)> {
        let mut r = Reader::new(&self.data[body..]);
        r.skip_white_spaces_and_comments();
        let d = r.read_with_context::<Dict<'_>>(&ReaderContext::dummy())?;
        Some((d, body + r.offset()))
    }

    /// The data of the stream whose dictionary `d` ends at `after`, as the
    /// parser reads it: `/Length` bytes after `stream` and its end of line,
    /// where `endstream` follows them, else up to the first `endstream`.
    /// `/Length` given by reference is resolved where `resolve` is (to the
    /// object last written with its number), as the parser does reading an
    /// object stream; reading a cross-reference stream, it does not.
    fn stream_data(
        &mut self,
        d: &Dict<'a>,
        after: usize,
        resolve: bool,
    ) -> Result<Option<&'a [u8]>, TooLarge> {
        let data = self.data;
        if d.contains_key(b"F") {
            return Ok(None);
        }
        let length = match d.get_raw::<usize>(b"Length") {
            Some(MaybeRef::NotRef(n)) => Some(n),
            Some(MaybeRef::Ref(_)) if resolve => self.usize_of(d, b"Length"),
            _ => None,
        }
        .filter(|n| u32::try_from(*n).is_ok());
        let start_of_data = |r: &mut Reader<'a>| -> Option<()> {
            r.forward_tag(b"stream")?;
            r.forward_tag(b"\n")
                .or_else(|| r.forward_tag(b"\r\n"))
                .or_else(|| r.forward_tag(b"\r"))
        };
        if let Some(length) = length {
            let mut r = Reader::new(&data[after..]);
            r.skip_white_spaces_and_comments();
            if start_of_data(&mut r).is_some() {
                if let Some(raw) = r.read_bytes(length) {
                    r.skip_white_spaces();
                    if r.forward_tag(b"endstream").is_some() {
                        self.scan_spent(r.offset())?;
                        return Ok(Some(raw));
                    }
                }
            }
        }
        // As the parser falls back to: the first `stream`, then up to the
        // first `endstream`, white space trimmed. Spent as far as it looks.
        let tail = &data[after..];
        let Some(s) = memchr::memmem::find(tail, b"stream") else {
            self.scan_spent(tail.len())?;
            return Ok(None);
        };
        let mut r = Reader::new(&tail[s..]);
        if start_of_data(&mut r).is_none() {
            self.scan_spent(s)?;
            return Ok(None);
        }
        let begin = s + r.offset();
        let Some(e) = memchr::memmem::find(&tail[begin..], b"endstream") else {
            self.scan_spent(tail.len())?;
            return Ok(None);
        };
        self.scan_spent(begin + e)?;
        let mut end = begin + e;
        while end > begin && is_white(tail[end - 1]) {
            end -= 1;
        }
        Ok(Some(&tail[begin..end]))
    }

    /// A cross-reference stream whose body starts at `body`, read as the
    /// parser reads one (its `/W`, `/Size` and `/Index` given in it): the
    /// objects each type 2 entry places in an object stream, by index, into
    /// [`Measure::packed`]. Its entries are counted against
    /// [`MAX_XREF_ENTRIES`], each one an entry the parser keeps.
    fn xref_stream(&mut self, body: usize) -> Result<(), TooLarge> {
        let Some((d, after)) = self.stream_dict(body) else {
            return Ok(());
        };
        let (Some(size), Some([f1, f2, f3])) = (d.get::<u32>(b"Size"), d.get::<[u8; 3]>(b"W"))
        else {
            return Ok(());
        };
        if f2 > 8 {
            return Ok(());
        }
        let Some(raw) = self.stream_data(&d, after, false)? else {
            return Ok(());
        };
        let Some(rows) = self.decode(&d, Cow::Borrowed(raw))? else {
            return Ok(());
        };
        let sections: Vec<(u32, u32)> = match d.get::<Array<'_>>(b"Index") {
            Some(a) => a.iter::<(u32, u32)>().collect(),
            None => vec![(0, size)],
        };
        let mut r = Reader::new(&rows);
        for (start, count) in sections {
            for i in 0..count {
                // The parser reads a type of one byte, whatever `/W` says.
                let kind = if f1 == 0 {
                    1
                } else {
                    match r.read_bytes(1) {
                        Some(b) => b[0],
                        None => return Ok(()),
                    }
                };
                let field = |r: &mut Reader<'_>, len: u8| -> Option<u32> {
                    xref_number(r.read_bytes(usize::from(len))?)
                };
                let (stream, index) = match kind {
                    0 => {
                        if r.skip_bytes(usize::from(f2) + usize::from(f3)).is_none() {
                            return Ok(());
                        }
                        continue;
                    }
                    1 => {
                        let ok = (f2 == 0 || field(&mut r, f2).is_some())
                            && (f3 == 0 || field(&mut r, f3).is_some());
                        if !ok {
                            return Ok(());
                        }
                        self.entry()?;
                        continue;
                    }
                    2 => {
                        let Some(stream) = field(&mut r, f2) else {
                            return Ok(());
                        };
                        let index = if f3 == 0 { Some(0) } else { field(&mut r, f3) };
                        let Some(index) = index else {
                            return Ok(());
                        };
                        (stream, index)
                    }
                    _ => return Ok(()),
                };
                self.entry()?;
                self.packed
                    .entry(stream as i32)
                    .or_default()
                    .push((start.wrapping_add(i) as i32, index));
            }
        }
        Ok(())
    }

    /// Count an entry of a cross-reference stream: past
    /// [`MAX_XREF_ENTRIES`], the file is refused.
    fn entry(&mut self) -> Result<(), TooLarge> {
        self.entries += 1;
        if self.entries > MAX_XREF_ENTRIES {
            return Err(TooLarge::fixed(format!(
                "its cross-reference streams list more than {MAX_XREF_ENTRIES} objects"
            )));
        }
        Ok(())
    }

    /// An object stream whose body starts at `body`, numbered `numbers`:
    /// decoded as the parser decodes it, and each object it holds read in
    /// place, under each number the parser may read it by.
    ///
    /// The parser finds an object of an object stream by its index there,
    /// which an entry of a cross-reference stream gives, whatever number
    /// the stream's header writes at that index; only when it rebuilds a
    /// cross-reference table it cannot read does it go by the header's
    /// numbers. Each object is numbered both ways.
    ///
    /// The stream is read with its `/Length` resolved, as the parser reads
    /// it by its number. Rebuilding a table, it reads it without, up to the
    /// first `endstream`: no further than with it, so the data it decodes
    /// then begins what is decoded here, and no object of it is missed.
    fn object_stream(&mut self, body: usize, numbers: &[i32]) -> Result<(), TooLarge> {
        let Some((d, after)) = self.stream_dict(body) else {
            return Ok(());
        };
        let Some(raw) = self.stream_data(&d, after, true)? else {
            return Ok(());
        };
        let (Some(n), Some(first)) = (self.usize_of(&d, b"N"), self.usize_of(&d, b"First")) else {
            return Ok(());
        };
        let Some(decoded) = self.decode(&d, Cow::Borrowed(raw))? else {
            return Ok(());
        };
        // The header, read from the start as the parser reads it: `n`
        // pairs of a number and an offset from `first`, all or nothing,
        // each pair at its index (an offset past the end, `usize::MAX`).
        let mut r = Reader::new(&decoded);
        let mut entries: Vec<(i32, usize)> = Vec::new();
        for _ in 0..n {
            r.skip_white_spaces_and_comments();
            let Some(num) = r.read_without_context::<u32>() else {
                return Ok(());
            };
            r.skip_white_spaces_and_comments();
            let Some(off) = r.read_without_context::<usize>() else {
                return Ok(());
            };
            let at = first.checked_add(off).filter(|at| *at <= decoded.len());
            entries.push((num as i32, at.unwrap_or(usize::MAX)));
        }
        // Each object read in place, up to the next offset: in a stream
        // written as the format says, its objects follow one another, and
        // each is read once however many indexes list it.
        let mut order: Vec<u32> = (0..entries.len() as u32)
            .filter(|i| entries[*i as usize].1 != usize::MAX)
            .collect();
        order.sort_unstable_by_key(|i| entries[*i as usize].1);
        let mut at_index: Vec<(Option<u32>, bool)> = vec![(None, false); entries.len()];
        let mut k = 0;
        while k < order.len() {
            let at = entries[order[k] as usize].1;
            let same = order[k..].partition_point(|i| entries[*i as usize].1 == at);
            let end = order
                .get(k + same)
                .map_or(decoded.len(), |i| entries[*i as usize].1);
            let seen = match self.read(&decoded, at, end)? {
                Some((object, _)) => (
                    self.graph.add(&object, 0).map(|id| id as u32),
                    matches!(object, Object::Dict(_)),
                ),
                None => (None, false),
            };
            for i in &order[k..k + same] {
                at_index[*i as usize] = seen;
            }
            k += same;
        }
        let at_index = |i: usize| {
            at_index
                .get(i)
                .map(|(node, dict)| (node.map(|n| n as usize), *dict))
        };
        // By the header's numbers, as a rebuilt table reads them.
        for (i, &(num, _)) in entries.iter().enumerate() {
            if let Some((node, dict)) = at_index(i) {
                self.graph.number(num, node, dict);
            }
        }
        // By the numbers the cross-reference streams give each index.
        for s in numbers {
            let Some(packed) = self.packed.get(s) else {
                continue;
            };
            for &(num, index) in packed {
                if let Some((node, dict)) = at_index(index as usize) {
                    self.graph.number(num, node, dict);
                }
            }
        }
        Ok(())
    }

    /// The value of `key` in `d`, a non-negative integer, given there or by
    /// reference to an object written in the file.
    fn usize_of(&mut self, d: &Dict<'_>, key: &[u8]) -> Option<usize> {
        match d.get_raw::<usize>(key)? {
            MaybeRef::NotRef(n) => Some(n),
            MaybeRef::Ref(r) => {
                let at = *self.last.get(&r.obj_number)?;
                let mut reader = Reader::new(&self.data[at..]);
                reader.skip_white_spaces_and_comments();
                reader.read_without_context::<usize>()
            }
        }
    }

    /// The value of `key` in `d`, given there or by reference to an object
    /// written in the file, the last written with its number.
    fn value(&mut self, d: &Dict<'a>, key: &[u8]) -> Result<Option<Object<'a>>, TooLarge> {
        Ok(match d.get_raw::<Object<'_>>(key) {
            None => None,
            Some(MaybeRef::NotRef(o)) => Some(o),
            Some(MaybeRef::Ref(r)) => match self.last.get(&r.obj_number) {
                Some(&at) => self.read(self.data, at, self.data.len())?.map(|(o, _)| o),
                None => None,
            },
        })
    }

    /// The filters of `d` and their parameters, as the parser lists them: a
    /// name, or an array of names whose parameters are an array alongside;
    /// a filter whose name it does not know is left out.
    fn filters(&mut self, d: &Dict<'a>) -> Result<Vec<(Filter, Option<Dict<'a>>)>, TooLarge> {
        let params = |o: Option<Object<'a>>| match o {
            Some(Object::Dict(p)) => Some(p),
            _ => None,
        };
        let parms = self.value(d, b"DecodeParms")?;
        Ok(match self.value(d, b"Filter")? {
            Some(Object::Name(n)) => Filter::from_name(&n)
                .map(|f| (f, params(parms)))
                .into_iter()
                .collect(),
            Some(Object::Array(names)) => {
                let mut each = match parms {
                    Some(Object::Array(a)) => a.iter::<Object<'_>>().collect::<Vec<_>>(),
                    _ => Vec::new(),
                }
                .into_iter();
                names
                    .iter::<Object<'_>>()
                    .filter_map(|n| {
                        let p = params(each.next());
                        match n {
                            Object::Name(n) => Filter::from_name(&n).map(|f| (f, p)),
                            _ => None,
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        })
    }

    /// The data `raw` of a stream of dictionary `d`, decoded by the
    /// parser's own filters, one at a time, each stage's output bounded
    /// before it runs: within [`MAX_OBJECT_STREAM_BYTES`] and what is left
    /// of [`MAX_OBJECT_STREAMS_BYTES`], or the file is refused. `None`
    /// where the parser would not decode it either, or a filter is one the
    /// measure does not run (an image filter, `Crypt`).
    fn decode(
        &mut self,
        d: &Dict<'a>,
        raw: Cow<'a, [u8]>,
    ) -> Result<Option<Cow<'a, [u8]>>, TooLarge> {
        let stages = self.filters(d)?;
        let mut data = raw;
        for (filter, params) in stages {
            let bound = match filter {
                Filter::AsciiHex => data.len() as u64 / 2 + 1,
                Filter::Ascii85 => ascii85_bound(&data),
                Filter::RunLength => run_length_len(&data),
                Filter::Flate => flate_bound(&data, MAX_OBJECT_STREAM_BYTES),
                Filter::Lzw => lzw_bound(&data, MAX_OBJECT_STREAM_BYTES),
                Filter::Other => return Ok(None),
            };
            if bound > MAX_OBJECT_STREAM_BYTES {
                return Err(TooLarge::fixed(format!(
                    "an object or cross-reference stream decodes past {} MiB",
                    MAX_OBJECT_STREAM_BYTES >> 20
                )));
            }
            let Some(next) = stage(filter, params.as_ref(), &data) else {
                return Ok(None);
            };
            self.decoded = self.decoded.saturating_add(next.len() as u64);
            if self.decoded > MAX_OBJECT_STREAMS_BYTES {
                return Err(TooLarge::fixed(format!(
                    "its object and cross-reference streams decode past {} MiB",
                    MAX_OBJECT_STREAMS_BYTES >> 20
                )));
            }
            data = Cow::Owned(next);
        }
        Ok(Some(data))
    }
}

/// A field of a cross-reference stream's entry, as the parser reads one:
/// big-endian, of 1 to 4 bytes or 8 (fitting 32 bits).
fn xref_number(b: &[u8]) -> Option<u32> {
    match b.len() {
        1..=4 => Some(b.iter().fold(0u32, |n, x| (n << 8) | u32::from(*x))),
        8 => u32::try_from(u64::from_be_bytes(b.try_into().ok()?)).ok(),
        _ => None,
    }
}

/// `data` decoded by one filter of the parser's, with `params`: a stream
/// written with that filter alone, decoded as the parser decodes one.
fn stage(filter: Filter, params: Option<&Dict<'_>>, data: &[u8]) -> Option<Vec<u8>> {
    let mut written = format!("<< /Filter /{}", filter.name()).into_bytes();
    if let Some(p) = params {
        // As written, `<<` and `>>` included.
        written.extend(b" /DecodeParms ");
        written.extend(p.data());
    }
    written.extend(format!(" /Length {} >>\nstream\n", data.len()).bytes());
    written.extend(data);
    written.extend(b"\nendstream\n");
    let mut r = Reader::new(&written);
    let stream = r.read_with_context::<Stream<'_>>(&ReaderContext::dummy())?;
    stream.decoded().ok().map(Cow::into_owned)
}

/// The most bytes ASCII85 decodes `data` to: four for each `z`, four for
/// each five other characters, rounded up.
fn ascii85_bound(data: &[u8]) -> u64 {
    let z = data.iter().filter(|b| **b == b'z').count() as u64;
    4 * z + (data.len() as u64 - z).div_ceil(5) * 4
}

/// What RunLength decodes `data` to, run by run.
fn run_length_len(data: &[u8]) -> u64 {
    let (mut i, mut out) = (0usize, 0u64);
    while let Some(&n) = data.get(i) {
        match n {
            128 => break,
            0..=127 => {
                out += u64::from(n) + 1;
                i += usize::from(n) + 2;
            }
            _ => {
                out += 257 - u64::from(n);
                i += 2;
            }
        }
    }
    out
}

/// The most bytes the parser's Flate decodes `data` to, counted by
/// inflating it to nowhere, up to `cap` and one more: as a zlib stream,
/// else as raw deflate, as the parser tries them. A stream broken for
/// both the parser reads on with a decoder of its own, which stops at the
/// first error too: bounded by what either inflated before failing, and a
/// window's margin.
fn flate_bound(data: &[u8], cap: u64) -> u64 {
    /// What `r` inflates to before it ends or fails, up to `cap` and one
    /// more; and whether it ended cleanly.
    fn count(mut r: impl Read, cap: u64) -> (u64, bool) {
        let mut buf = vec![0u8; 64 << 10];
        let mut n = 0u64;
        loop {
            match r.read(&mut buf) {
                Ok(0) => return (n, true),
                Ok(k) => n += k as u64,
                Err(_) => return (n, false),
            }
            if n > cap {
                return (n, true);
            }
        }
    }
    let (zlib, ok) = count(flate2::read::ZlibDecoder::new(data), cap);
    if ok {
        return zlib;
    }
    let (raw, ok) = count(flate2::read::DeflateDecoder::new(data), cap);
    if ok {
        return raw;
    }
    zlib.max(raw) + (1 << 20)
}

/// The most bytes the parser's LZW decodes `data` to, either early change,
/// counted by running its table on lengths alone, up to `cap` and one more.
fn lzw_bound(data: &[u8], cap: u64) -> u64 {
    lzw_len(data, true, cap).max(lzw_len(data, false, cap))
}

/// What LZW decodes `data` to with `early` change, as the parser's decoder
/// runs (its table of up to 4,096 entries, cleared by code 256, ended by
/// 257), the length of each entry kept instead of its bytes.
fn lzw_len(data: &[u8], early: bool, cap: u64) -> u64 {
    const CLEAR: usize = 256;
    const EOD: usize = 257;
    const MAX_ENTRIES: usize = 4096;
    let mut lengths: Vec<u32> = vec![1; 258];
    let width = |len: usize| -> u8 {
        let adjusted = len + usize::from(early);
        match adjusted {
            a if a >= 2048 => 12,
            a if a >= 1024 => 11,
            a if a >= 512 => 10,
            _ => 9,
        }
    };
    let mut reader = BitReader::new(data);
    let mut bits = width(lengths.len());
    let mut prev: Option<usize> = None;
    let mut out = 0u64;
    while out <= cap {
        let Some(code) = reader.read(bits) else {
            break;
        };
        let code = code as usize;
        match code {
            CLEAR => {
                lengths.truncate(258);
                prev = None;
            }
            EOD => break,
            new => {
                let size = lengths.len();
                if new > size {
                    return 0;
                }
                if new < size {
                    out += u64::from(lengths[new]);
                    if let Some(p) = prev {
                        if size < MAX_ENTRIES {
                            lengths.push(lengths[p] + 1);
                        }
                    }
                } else if let Some(p) = prev {
                    if size >= MAX_ENTRIES {
                        return 0;
                    }
                    lengths.push(lengths[p] + 1);
                    out += u64::from(lengths[p] + 1);
                } else {
                    return 0;
                }
                prev = Some(new);
            }
        }
        bits = width(lengths.len());
    }
    out
}

/// A node of the graph the tree is measured on: a dictionary with `/Kids`
/// (a `/Pages` node is a level), or an array that may be a node's kids.
struct Node {
    level: bool,
    kids: Vec<Kid>,
    /// An object written in the file, not in an object stream nor inline:
    /// the parser keeps it on its path as it reads it by its number, and
    /// cuts a cycle where it meets it again. An object read from an object
    /// stream is not kept, and a cycle of those only it walks forever.
    cuts: bool,
}

/// What a node's kids name.
enum Kid {
    /// An object: every version of it.
    Object(i32),
    /// A node written inline.
    Inline(usize),
    /// A dictionary written inline with no kids: a page.
    Page,
}

#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    /// Each version of each object that is a node.
    objects: HashMap<i32, Vec<usize>>,
    /// The objects some version of which is a dictionary: a page, where a
    /// kid names one.
    dicts: HashSet<i32>,
    /// The objects a dictionary names by `/Pages`: where the parser's walk
    /// starts, from the catalog it reads, whichever version that is.
    roots: HashSet<i32>,
}

impl Graph {
    /// Add `object`: the node it is, if it is one.
    fn add(&mut self, object: &Object<'_>, nesting: usize) -> Option<usize> {
        let node = match object {
            Object::Dict(d) => self.dict(d, nesting)?,
            Object::Stream(s) => self.dict(s.dict(), nesting)?,
            Object::Array(a) => Node {
                level: false,
                kids: self.entries(a, nesting),
                cuts: false,
            },
            _ => return None,
        };
        let id = self.nodes.len();
        self.nodes.push(node);
        Some(id)
    }

    /// Number `num` a version of `node`, where the object is one, and of a
    /// dictionary, where it is one.
    fn number(&mut self, num: i32, node: Option<usize>, dict: bool) {
        if let Some(id) = node {
            self.objects.entry(num).or_default().push(id);
        }
        if dict {
            self.dicts.insert(num);
        }
    }

    /// A dictionary as hayro-syntax walks it: a level when its `/Type` is
    /// `/Pages`, or a reference that may resolve to it; its `/Kids` either
    /// way, as the root is walked whatever its type.
    fn dict(&mut self, d: &Dict<'_>, nesting: usize) -> Option<Node> {
        // A catalog names the root by reference (the parser takes no other):
        // any dictionary that does is taken for one.
        if let Some(MaybeRef::Ref(r)) = d.get_raw::<Object<'_>>(b"Pages") {
            self.roots.insert(r.obj_number);
        }
        let level = match d.get_raw::<Name<'_>>(b"Type") {
            Some(MaybeRef::NotRef(n)) => &*n == b"Pages",
            Some(MaybeRef::Ref(_)) => true,
            None => false,
        };
        let kids = match d.get_raw::<Object<'_>>(b"Kids")? {
            MaybeRef::Ref(r) => vec![Kid::Object(r.obj_number)],
            MaybeRef::NotRef(Object::Array(a)) => self.entries(&a, nesting),
            MaybeRef::NotRef(_) => Vec::new(),
        };
        Some(Node {
            level,
            kids,
            cuts: false,
        })
    }

    /// The kids an array names: the objects it references, and the nodes
    /// and pages written in it.
    fn entries(&mut self, a: &Array<'_>, nesting: usize) -> Vec<Kid> {
        let mut kids = Vec::new();
        for entry in crate::array_entries(a) {
            match entry {
                MaybeRef::Ref(r) => kids.push(Kid::Object(r.obj_number)),
                // Followed as deep as a tree may nest and one more, past
                // which it is refused anyway: the walk here recurses too.
                MaybeRef::NotRef(o @ Object::Dict(_)) if nesting <= MAX_PAGE_TREE_DEPTH => {
                    kids.push(self.add(&o, nesting + 1).map_or(Kid::Page, Kid::Inline))
                }
                MaybeRef::NotRef(Object::Dict(_)) => kids.push(Kid::Page),
                _ => {}
            }
        }
        kids
    }

    /// The nodes `v`'s kids name, and whether each is the dictionary of a
    /// page as well (a kid that is a dictionary the parser keeps as a page
    /// where it is not a `/Pages` node).
    fn kid_nodes(&self, v: usize) -> impl Iterator<Item = usize> + '_ {
        self.nodes[v].kids.iter().flat_map(move |k| {
            let nodes: &[usize] = match k {
                Kid::Inline(id) => std::slice::from_ref(id),
                Kid::Object(n) => self.objects.get(n).map_or(&[], Vec::as_slice),
                Kid::Page => &[],
            };
            nodes.iter().copied()
        })
    }

    /// The depth and the pages: no less than the parser's walk from the
    /// root makes. Those of the nodes a catalog names (each version), what
    /// they reach included; where no catalog is seen, the most of any node.
    ///
    /// Over the graph's strongly connected components, each measured after
    /// those it reaches. A node in no cycle has its own level and pages
    /// plus its kids'. A cycle is one the parser cuts where it meets an
    /// object already on its path, so a path through it visits each of its
    /// nodes once at most: its depth is all its levels plus the deepest it
    /// leads to, and its pages, those of every path through it, at most
    /// one for each kid it names, inside or out, times the paths, at most
    /// `b^0 + .. + b^(k-1)` for `k` nodes each naming at most `b` kids in
    /// it. A cycle of nodes the parser does not cut (objects of object
    /// streams) is walked forever: past every bound.
    fn shape(&self) -> Shape {
        let n = self.nodes.len();
        let mut depth = vec![0usize; n];
        let mut pages = vec![0u64; n];
        let mut shape = Shape { depth: 0, pages: 0 };
        let mut component = vec![usize::MAX; n];
        let all: Vec<usize> = (0..n).collect();
        for (c, members) in self.components(&all, n).into_iter().enumerate() {
            for &v in &members {
                component[v] = c;
            }
            // A cycle the parser does not cut, among the component's nodes
            // it does not keep on its path.
            let loose: Vec<usize> = members
                .iter()
                .copied()
                .filter(|v| !self.nodes[*v].cuts)
                .collect();
            let endless = match loose.as_slice() {
                [] => false,
                [v] => self.kid_nodes(*v).any(|w| w == *v),
                _ => self
                    .components(&loose, n)
                    .iter()
                    .any(|l| l.len() > 1 || self.kid_nodes(l[0]).any(|w| w == l[0])),
            };
            if endless {
                for &v in &members {
                    pages[v] = u64::MAX;
                    depth[v] = usize::MAX;
                }
                shape = Shape {
                    depth: usize::MAX,
                    pages: u64::MAX,
                };
                continue;
            }
            // What a kid outside the component brings: its pages (a page
            // itself when no version of it is a node with kids of its own),
            // its depth.
            let outside = |k: &Kid| -> (u64, usize) {
                match k {
                    Kid::Page => (1, 0),
                    Kid::Inline(id) if component[*id] != c => (pages[*id].max(1), depth[*id]),
                    Kid::Inline(_) => (0, 0),
                    Kid::Object(num) => {
                        let versions = self.objects.get(num).map_or(&[][..], Vec::as_slice);
                        let mut p = u64::from(self.dicts.contains(num));
                        let mut d = 0;
                        for &w in versions.iter().filter(|w| component[**w] != c) {
                            p = p.max(pages[w]);
                            d = d.max(depth[w]);
                        }
                        (p, d)
                    }
                }
            };
            let inside = |k: &Kid| -> u64 {
                match k {
                    Kid::Inline(id) => u64::from(component[*id] == c),
                    Kid::Object(num) => u64::from(
                        self.objects
                            .get(num)
                            .is_some_and(|vs| vs.iter().any(|w| component[*w] == c)),
                    ),
                    Kid::Page => 0,
                }
            };
            if members.len() == 1 {
                let v = members[0];
                let mut p = 0u64;
                let mut d = 0usize;
                for k in &self.nodes[v].kids {
                    let (kp, kd) = outside(k);
                    p = p.saturating_add(kp);
                    d = d.max(kd);
                }
                pages[v] = p;
                depth[v] = d.saturating_add(usize::from(self.nodes[v].level));
            } else {
                let levels = members.iter().filter(|v| self.nodes[**v].level).count();
                let mut reach = 0usize;
                let mut each = 0u64;
                let mut branching = 0u64;
                for &v in &members {
                    let mut within = 0u64;
                    for k in &self.nodes[v].kids {
                        let (kp, kd) = outside(k);
                        each = each.saturating_add(kp).saturating_add(inside(k));
                        reach = reach.max(kd);
                        within += inside(k);
                    }
                    branching = branching.max(within);
                }
                let mut paths = 0u64;
                let mut term = 1u64;
                for _ in 0..members.len() {
                    paths = paths.saturating_add(term);
                    term = term.saturating_mul(branching);
                    if term == 0 || paths == u64::MAX {
                        break;
                    }
                }
                let p = paths.saturating_mul(each);
                for &v in &members {
                    pages[v] = p;
                    depth[v] = levels.saturating_add(reach);
                }
            }
            for &v in &members {
                shape.depth = shape.depth.max(depth[v]);
                shape.pages = shape.pages.max(pages[v]);
            }
        }
        if self.roots.is_empty() {
            return shape;
        }
        // A node's depth and pages are those of everything it reaches: the
        // roots' are the walk's.
        let mut walked = Shape { depth: 0, pages: 0 };
        for &v in self
            .roots
            .iter()
            .filter_map(|n| self.objects.get(n))
            .flatten()
        {
            walked.depth = walked.depth.max(depth[v]);
            walked.pages = walked.pages.max(pages[v]);
        }
        walked
    }

    /// The strongly connected components of the graph `nodes` (of the `n`
    /// the graph has) make among themselves, each listed after every one
    /// it reaches (Tarjan's, without recursion), in time linear in them.
    fn components(&self, nodes: &[usize], n: usize) -> Vec<Vec<usize>> {
        // Where each node is in `nodes`: a node's own index where they are
        // the whole graph, else looked up.
        let whole = nodes.len() == n;
        let at: HashMap<usize, usize> = if whole {
            HashMap::new()
        } else {
            nodes.iter().enumerate().map(|(i, v)| (*v, i)).collect()
        };
        let local = |v: usize| if whole { Some(v) } else { at.get(&v).copied() };
        let kids_of = |v: usize| -> Vec<usize> { self.kid_nodes(v).filter_map(local).collect() };
        let m = nodes.len();
        let mut index = vec![usize::MAX; m];
        let mut low = vec![0usize; m];
        let mut on_stack = vec![false; m];
        let mut stack = Vec::new();
        let mut out = Vec::new();
        let mut next = 0;
        for root in 0..m {
            if index[root] != usize::MAX {
                continue;
            }
            let mut path: Vec<(usize, Vec<usize>, usize)> = Vec::new();
            index[root] = next;
            low[root] = next;
            next += 1;
            stack.push(root);
            on_stack[root] = true;
            path.push((root, kids_of(nodes[root]), 0));
            while let Some((v, kids, i)) = path.last_mut() {
                let v = *v;
                if let Some(&w) = kids.get(*i) {
                    *i += 1;
                    if index[w] == usize::MAX {
                        index[w] = next;
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        path.push((w, kids_of(nodes[w]), 0));
                    } else if on_stack[w] {
                        low[v] = low[v].min(index[w]);
                    }
                    continue;
                }
                path.pop();
                if let Some((u, _, _)) = path.last() {
                    low[*u] = low[*u].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut members = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        members.push(nodes[w]);
                        if w == v {
                            break;
                        }
                    }
                    out.push(members);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of `objects`, numbered from 1, with a cross-reference table;
    /// each object's header written by `header`.
    fn pdf_with(objects: &[String], header: impl Fn(usize) -> String) -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{}{o}\nendobj", header(i + 1)).bytes());
        }
        let xref = out.len();
        out.extend(format!("\nxref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        out
    }

    /// A PDF of `objects`, numbered from 1, each `N 0 obj` on a line.
    fn pdf(objects: &[String]) -> Vec<u8> {
        pdf_with(objects, |n| format!("\n{n} 0 obj\n"))
    }

    /// The objects of a page tree `depth` `/Pages` nodes deep over one
    /// page, each node an object of its own.
    fn chain_objects(depth: usize) -> Vec<String> {
        let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
        for i in 0..depth {
            objects.push(format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", i + 3));
        }
        objects.push("<< /Type /Page /MediaBox [0 0 612 792] >>".to_string());
        objects
    }

    fn chain(depth: usize) -> Vec<u8> {
        pdf(&chain_objects(depth))
    }

    fn depth(data: &[u8]) -> usize {
        measure(data).unwrap().depth
    }

    fn pages(data: &[u8]) -> u64 {
        measure(data).unwrap().pages
    }

    fn refused(data: Vec<u8>) -> String {
        match open(data) {
            Err(ExtractError::TooLarge(e)) => {
                assert_eq!(e.limit(), fluree_doc_model::budget::Limit::Fixed);
                e.to_string()
            }
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("opened"),
        }
    }

    #[test]
    fn the_depth_is_the_most_pages_nodes_on_one_path() {
        assert_eq!(depth(&chain(1)), 1);
        assert_eq!(depth(&chain(40)), 40);
        // A balanced tree of two levels, and nothing else that is a level.
        let tree = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 4 >>".into(),
            "<< /Type /Pages /Kids [5 0 R 5 0 R] /Count 2 >>".into(),
            "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".into(),
            "<< /Type /Page /MediaBox [0 0 612 792] /Annots [1 0 R] >>".into(),
        ]);
        assert_eq!(measure(&tree).unwrap(), Shape { depth: 2, pages: 3 });
        assert_eq!(open(tree).unwrap().pages().len(), 3);
    }

    #[test]
    fn a_tree_past_the_bound_is_refused_before_it_is_walked() {
        assert!(open(chain(MAX_PAGE_TREE_DEPTH)).is_ok());
        let e = refused(chain(MAX_PAGE_TREE_DEPTH + 1));
        assert!(e.contains("nests more than 256 deep"), "{e}");
        // 20,000 deep overflowed the parser's stack: refused, measured
        // without recursion.
        refused(chain(20_000));
        assert!(matches!(
            crate::extract_bytes(chain(20_000)),
            Err(ExtractError::TooLarge(_))
        ));
    }

    #[test]
    fn kids_are_followed_inline_through_arrays_and_to_every_version() {
        // Nodes written inline in their parent's kids.
        let mut inline = "<< /Type /Page >>".to_string();
        for _ in 0..30 {
            inline = format!("<< /Type /Pages /Kids [{inline}] >>");
        }
        let tree = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!("<< /Type /Pages /Kids [{inline}] >>"),
        ]);
        assert_eq!(
            measure(&tree).unwrap(),
            Shape {
                depth: 31,
                pages: 1
            }
        );
        // Kids an indirect array names; a type given by reference.
        let tree = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids 3 0 R >>".into(),
            "[4 0 R]".into(),
            "<< /Type 5 0 R /Kids [6 0 R] >>".into(),
            "/Pages".into(),
            "<< /Type /Page >>".into(),
        ]);
        assert_eq!(depth(&tree), 2);
        // A later version of an object, an update appended, is followed too.
        let mut updated = chain(3);
        updated.extend(b"5 0 obj\n<< /Type /Pages /Kids [9 0 R] >>\nendobj\n");
        updated.extend(b"9 0 obj\n<< /Type /Pages /Kids [10 0 R] >>\nendobj\n");
        assert_eq!(depth(&updated), 5);
        // A loop is walked once.
        let looped = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 2 0 R] >>".into(),
            "<< /Type /Pages /Kids [2 0 R] >>".into(),
        ]);
        assert_eq!(depth(&looped), 2);
        assert!(open(looped).is_ok());
    }

    #[test]
    fn a_header_is_read_as_the_parsers_lexer_reads_it() {
        // Each object glued to the one before it, `endobj3 0 obj`.
        let glued = pdf_with(&chain_objects(300), |n| format!("{n} 0 obj"));
        assert!(glued.windows(9).any(|w| w == b"endobj3 0"));
        assert_eq!(depth(&glued), 300);
        // A comment between the generation and `obj`, or between the
        // number and the generation, and white space of every kind.
        let commented = pdf_with(&chain_objects(300), |n| format!("\n{n} 0 %c\nobj "));
        assert_eq!(depth(&commented), 300);
        let commented = pdf_with(&chain_objects(300), |n| {
            format!("\n{n}%a\n%b obj\r\n0\t%c\r\n\x0cobj\x00")
        });
        assert_eq!(depth(&commented), 300);
        // The parser reads each: past the bound, each is refused.
        for header in [
            |n: usize| format!("{n} 0 obj"),
            |n: usize| format!("\n{n} 0 %c\nobj "),
        ] {
            refused(pdf_with(&chain_objects(MAX_PAGE_TREE_DEPTH + 1), header));
        }
        // What the parser does not read as a header is not one: a number
        // glued to the generation or to `obj`, or a comment that runs past
        // `obj`.
        for header in [
            |n: usize| format!("\n{n}0 obj\n"),
            |n: usize| format!("\n{n} 0obj\n"),
            |n: usize| format!("\n{n} 0 %obj\n"),
        ] {
            assert_eq!(depth(&pdf_with(&chain_objects(40), header)), 0);
        }
    }

    #[test]
    fn the_numbers_before_obj_are_the_ones_the_lexer_reads() {
        let ends = |s: &str| {
            let at = s.find("obj").unwrap();
            let mut e = token_ends_before(s.as_bytes(), at);
            e.sort_unstable();
            e
        };
        assert_eq!(ends("1 0 obj"), [3]);
        assert_eq!(ends("1 0obj"), Vec::<usize>::new());
        // A comment ends at its line's end; any `%` on a line ending in
        // what is skipped may start it.
        assert_eq!(ends("5 %b 7 %c\n obj"), [1, 6]);
        assert_eq!(ends("5 %c obj"), Vec::<usize>::new());
        assert_eq!(ends("5\r\n%x\r%y\n\tobj"), [1]);
        let read = |s: &str| {
            let h = headers(s.as_bytes());
            h.into_iter().flat_map(|(_, n)| n).collect::<Vec<_>>()
        };
        assert_eq!(read("endobj12 0 obj"), [12]);
        assert_eq!(read("x 12 0 %a 7 %b\nobj"), [12]);
        assert_eq!(read("5 %b 7 %c\n0 obj"), [7, 5]);
        assert_eq!(read("+3 0 obj"), [3]);
        assert_eq!(read("3 0 endobj"), Vec::<i32>::new());
    }

    /// A tree whose root's kid heads a chain of `count` `/Pages` nodes
    /// (from 3), each the parent of the next, in one object stream coded
    /// by `filter` (its parameters `parms`), `encode`.
    fn packed_with(
        count: usize,
        filter: &str,
        parms: &str,
        encode: impl Fn(&[u8]) -> Vec<u8>,
    ) -> Vec<u8> {
        let base = 3;
        let bodies: Vec<String> = (0..count)
            .map(|i| format!("<< /Type /Pages /Kids [{} 0 R] >>", base + i + 1))
            .collect();
        let mut header = String::new();
        let mut off = 0;
        for (i, b) in bodies.iter().enumerate() {
            header.push_str(&format!("{} {off} ", base + i));
            off += b.len() + 1;
        }
        let body: String = bodies.iter().map(|b| format!("{b} ")).collect();
        let packed = encode(format!("{header}{body}").as_bytes());
        let mut file = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] >>".into(),
        ]);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N {count} /First {} /Filter {filter} {parms} /Length {} >>\nstream\n",
                header.len(),
                packed.len()
            )
            .bytes(),
        );
        file.extend(&packed);
        file.extend(b"\nendstream\nendobj\n");
        file
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, data).unwrap();
        z.finish().unwrap()
    }

    fn packed(count: usize) -> Vec<u8> {
        packed_with(count, "/FlateDecode", "", zlib)
    }

    #[test]
    fn nodes_in_an_object_stream_are_counted() {
        assert_eq!(depth(&packed(200)), 201);
        let deep = packed(MAX_PAGE_TREE_DEPTH + 10);
        assert!(depth(&deep) > MAX_PAGE_TREE_DEPTH);
        refused(deep);
    }

    /// `data` as LZW codes of nine bits: each byte a code of its own, the
    /// table cleared every 200 codes so that it stays nine bits wide.
    fn lzw(data: &[u8]) -> Vec<u8> {
        let mut codes = vec![256u16];
        for (i, b) in data.iter().enumerate() {
            if i > 0 && i % 200 == 0 {
                codes.push(256);
            }
            codes.push(u16::from(*b));
        }
        codes.push(257);
        let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
        for c in codes {
            acc = (acc << 9) | u32::from(c);
            bits += 9;
            while bits >= 8 {
                out.push((acc >> (bits - 8)) as u8);
                bits -= 8;
            }
        }
        if bits > 0 {
            out.push((acc << (8 - bits)) as u8);
        }
        out
    }

    /// Each row of eight bytes behind the PNG `Up` predictor, deflated.
    fn up_predicted(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev = [0u8; 8];
        for row in data.chunks(8) {
            let mut r = [b' '; 8];
            r[..row.len()].copy_from_slice(row);
            out.push(2);
            out.extend(r.iter().zip(prev).map(|(b, p)| b.wrapping_sub(p)));
            prev = r;
        }
        zlib(&out)
    }

    #[test]
    fn an_object_stream_is_decoded_by_the_parsers_filters() {
        let hex = |d: &[u8]| {
            let mut s: Vec<u8> = d
                .iter()
                .flat_map(|b| format!("{b:02x}").into_bytes())
                .collect();
            s.push(b'>');
            s
        };
        let deep = MAX_PAGE_TREE_DEPTH + 10;
        for (filter, parms, encode) in [
            ("[/FlateDecode]", "", &zlib as &dyn Fn(&[u8]) -> Vec<u8>),
            ("/ASCIIHexDecode", "", &hex),
            ("[/AHx /Fl]", "", &|d: &[u8]| hex(&zlib(d))),
            ("/LZWDecode", "", &lzw),
            (
                "/FlateDecode",
                "/DecodeParms << /Predictor 12 /Columns 8 >>",
                &up_predicted,
            ),
        ] {
            assert_eq!(
                depth(&packed_with(40, filter, parms, encode)),
                41,
                "{filter}"
            );
            let e = refused(packed_with(deep, filter, parms, encode));
            assert!(e.contains("nests more than"), "{filter}: {e}");
        }
        // An image filter is not one the measure runs, nor an unknown
        // filter the parser skips.
        assert_eq!(depth(&packed_with(40, "/DCTDecode", "", zlib)), 1);
        assert_eq!(depth(&packed_with(40, "[/Foo /Fl]", "", zlib)), 41);
    }

    #[test]
    fn lzw_is_counted_as_the_parser_decodes_it() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        let codes = lzw(&data);
        assert_eq!(stage(Filter::Lzw, None, &codes).unwrap(), data);
        assert_eq!(lzw_bound(&codes, u64::MAX), 5000);
        assert!(lzw_bound(&codes, 100) <= 102);
        // Codes naming entries of the table, one the entry it makes
        // (`aba`): a, b, ab, aba, ba.
        let mut packed = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in [256u32, 97, 98, 258, 260, 259, 257] {
            acc = (acc << 9) | c;
            bits += 9;
            while bits >= 8 {
                packed.push((acc >> (bits - 8)) as u8);
                bits -= 8;
            }
        }
        packed.push((acc << (8 - bits)) as u8);
        assert_eq!(stage(Filter::Lzw, None, &packed).unwrap(), b"ababababa");
        assert_eq!(lzw_bound(&packed, u64::MAX), 9);
        assert_eq!(run_length_len(&[2, b'a', b'b', b'c', 254, b'x', 128]), 6);
        assert_eq!(ascii85_bound(b"zzz"), 12);
    }

    #[test]
    fn an_object_stream_is_read_in_place() {
        // Its objects listed at one offset, over 6 MiB of padding: read
        // once each, not copied for each number (60 MiB of padding under
        // 200 numbers took 12 GB).
        let pad = " ".repeat(6 << 20);
        let header: String = (0..200).map(|i| format!("{} 0 ", 3 + i)).collect();
        let plain = format!("{header}<< /Type /Pages /Kids [3 0 R] >>{pad}");
        let packed = zlib(plain.as_bytes());
        let mut file = pdf(&["<< /Type /Catalog /Pages 3 0 R >>".into()]);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N 200 /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
                header.len(),
                packed.len()
            )
            .bytes(),
        );
        file.extend(&packed);
        file.extend(b"\nendstream\nendobj\n");
        // Each of the 200 names the one node, whose kid is itself: a cycle
        // the parser does not cut in an object stream, and walked forever
        // (60 MiB of padding aborted the process).
        let e = refused(file);
        assert!(e.contains("nests more than 256 deep"), "{e}");
        // Offsets out of order are read as well, each object up to the
        // next one in the stream.
        let bodies = [
            "<< /Type /Pages /Kids [4 0 R] >>",
            "<< /Type /Pages /Kids [5 0 R] >>",
        ];
        let header = format!("3 {} 4 0 ", bodies[1].len() + 1);
        let plain = format!("{header}{} {}", bodies[1], bodies[0]);
        let packed = zlib(plain.as_bytes());
        let mut file = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] >>".into(),
        ]);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N 2 /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
                header.len(),
                packed.len()
            )
            .bytes(),
        );
        file.extend(&packed);
        file.extend(b"\nendstream\nendobj\n");
        assert_eq!(depth(&file), 3);
    }

    #[test]
    fn a_page_named_many_times_is_counted_each_time() {
        // Two levels, each naming its one kid a thousand times: a million
        // pages from 12 KB.
        let fan = |kid: usize| {
            let kids = vec![format!("{kid} 0 R"); 1000].join(" ");
            format!("<< /Type /Pages /Kids [{kids}] >>")
        };
        let two = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            fan(3),
            fan(4),
            "<< /Type /Page >>".into(),
        ]);
        assert!(two.len() < 13_000);
        assert_eq!(
            measure(&two).unwrap(),
            Shape {
                depth: 2,
                pages: 1_000_000
            }
        );
        let e = refused(two);
        assert!(e.contains("lists more than 250000 pages"), "{e}");
        // 250 times 1000, at the bound, is opened.
        let at = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!(
                "<< /Type /Pages /Kids [{}] >>",
                vec!["3 0 R"; 250].join(" ")
            ),
            fan(4),
            "<< /Type /Page >>".into(),
        ]);
        assert_eq!(pages(&at), 250_000);
        assert_eq!(open(at).unwrap().pages().len(), 250_000);
        // Through a cycle, every path is counted.
        let cycle = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R 3 0 R 5 0 R] >>".into(),
            "<< /Type /Pages /Kids [2 0 R 2 0 R 5 0 R] >>".into(),
            "<< /Type /Page >>".into(),
            "<< /Type /Page >>".into(),
        ]);
        let s = measure(&cycle).unwrap();
        assert_eq!(s.depth, 2);
        // Counted at most (1 + 2) paths, times the ten kids the two nodes
        // name, each a page: no fewer than the parser keeps.
        assert_eq!(s.pages, 30);
        assert!(open(cycle).unwrap().pages().len() as u64 <= s.pages);
        // The same cycle in an object stream the parser walks forever.
        let mut packed = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] >>".into(),
        ]);
        let body = "<< /Type /Pages /Kids [4 0 R] >> << /Type /Pages /Kids [3 0 R] >>";
        let plain = format!("3 0 4 33 {body}");
        packed.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N 2 /First 9 /Length {} >>\nstream\n{plain}\nendstream\nendobj\n",
                plain.len()
            )
            .bytes(),
        );
        assert_eq!(measure(&packed).unwrap().depth, usize::MAX);
        refused(packed);
    }

    #[test]
    fn an_object_nesting_too_deep_for_the_parser_is_refused() {
        let nest = |n: usize| {
            format!(
                "<< /Type /Pages /Kids [3 0 R] /X {}1{} >>",
                "<< /A [".repeat(n),
                "] >>".repeat(n)
            )
        };
        let file = |n: usize| {
            pdf(&[
                "<< /Type /Catalog /Pages 2 0 R >>".into(),
                nest(n),
                "<< /Type /Page >>".into(),
            ])
        };
        assert!(open(file(100)).is_ok());
        assert!(open(file(1000)).is_ok());
        let e = refused(file(100_000));
        assert!(e.contains("an object nests more than 2048 deep"), "{e}");
        // Strings, comments and names hold no nesting.
        let quiet = pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            format!(
                "<< /Type /Pages /Kids [3 0 R] /S ({}) /H <41 42> /N /a%b\n /C 1 >>",
                "[\\(".repeat(1000)
            ),
            "<< /Type /Page >>".into(),
        ]);
        assert_eq!(measure(&quiet).unwrap(), Shape { depth: 1, pages: 1 });
    }

    #[test]
    fn only_what_a_catalog_reaches_is_measured() {
        // A structure tree's `/K` written as an array of its own, and a name
        // tree's node, each naming 300,000 dictionaries: past the pages a
        // tree may list, were they pages, and no page of the parser's walk.
        let refs = vec!["5 0 R"; 300_000].join(" ");
        let objects = |catalog: &str| -> Vec<String> {
            vec![
                catalog.into(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
                "<< /Type /Page /MediaBox [0 0 612 792] >>".into(),
                format!("[{refs}]"),
                "<< /S /P >>".into(),
                format!("<< /Kids [{refs}] >>"),
                "<< /Type /StructTreeRoot /K 4 0 R >>".into(),
            ]
        };
        let tagged = pdf(&objects(
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R /Names << /Dests 6 0 R >> >>",
        ));
        assert_eq!(measure(&tagged).unwrap(), Shape { depth: 1, pages: 1 });
        assert_eq!(open(tagged).unwrap().pages().len(), 1);
        // Where no catalog is seen, every node is measured, as the parser
        // may start from any.
        let bare = pdf(&objects("<< /Type /Catalog >>"));
        assert_eq!(pages(&bare), 300_000);
        // Every version of a catalog, and what each names, is a root.
        let mut updated = pdf(&objects(
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 7 0 R >>",
        ));
        updated.extend(b"1 0 obj\n<< /Type /Catalog /Pages 6 0 R >>\nendobj\n");
        assert_eq!(pages(&updated), 300_000);
    }

    #[test]
    fn an_object_stream_s_objects_are_scanned_within_what_it_decodes_to() {
        // 3,000 objects of a thousand bytes each, packed in a stream of a
        // few kilobytes: 3 MB scanned, more than sixteen passes over the
        // file, and once each over what the stream decodes to.
        let bodies: Vec<String> = (0..3000)
            .map(|_| format!("<< /P /{} >>", "a".repeat(1000)))
            .collect();
        let mut header = String::new();
        let mut off = 0;
        for (i, b) in bodies.iter().enumerate() {
            header.push_str(&format!("{} {off} ", 10 + i));
            off += b.len() + 1;
        }
        let body: String = bodies.iter().map(|b| format!("{b} ")).collect();
        let packed = zlib(format!("{header}{body}").as_bytes());
        let mut file = chain(3);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N 3000 /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
                header.len(),
                packed.len()
            )
            .bytes(),
        );
        file.extend(&packed);
        file.extend(b"\nendstream\nendobj\n");
        assert!(16 * file.len() + (1 << 20) < 3_000_000, "{}", file.len());
        assert_eq!(depth(&file), 3);
    }

    #[test]
    fn object_streams_decoding_past_their_bound_are_refused() {
        let zeros = vec![b' '; (MAX_OBJECT_STREAM_BYTES + 1) as usize];
        let bomb = zlib(&zeros);
        let mut file = chain(3);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
                bomb.len()
            )
            .bytes(),
        );
        file.extend(&bomb);
        file.extend(b"\nendstream\nendobj\n");
        let e = refused(file);
        assert!(
            e.contains("an object or cross-reference stream decodes past 64 MiB"),
            "{e}"
        );
    }

    /// How [`indexed`] writes its file.
    #[derive(Clone, Copy, Default)]
    struct Indexed {
        /// The object stream's `/Length` given by reference.
        length_by_ref: bool,
        /// The object stream stored, not compressed: its objects' bytes as
        /// written.
        stored: bool,
        /// A table of the objects written in the file, its trailer naming
        /// the cross-reference stream by `/XRefStm` (a hybrid file).
        hybrid: bool,
    }

    /// A file whose root (2) names object 3, `bodies` packed in one object
    /// stream (500) as objects 3, 4, ... by a cross-reference stream (501)
    /// that places object `3 + i` at index `i`, whatever number the
    /// stream's header writes there (`header(i)`).
    fn indexed(bodies: &[String], header: impl Fn(usize) -> usize, how: Indexed) -> Vec<u8> {
        let mut head = String::new();
        let mut off = 0;
        for (i, b) in bodies.iter().enumerate() {
            head.push_str(&format!("{} {off} ", header(i)));
            off += b.len() + 1;
        }
        let body: String = bodies.iter().map(|b| format!("{b} ")).collect();
        let plain = format!("{head}{body}");
        let level = if how.stored {
            flate2::Compression::none()
        } else {
            flate2::Compression::default()
        };
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), level);
        std::io::Write::write_all(&mut z, plain.as_bytes()).unwrap();
        let packed = z.finish().unwrap();
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut at: Vec<(u32, usize)> = Vec::new();
        let mut obj = |out: &mut Vec<u8>, n: u32, body: &[u8]| {
            at.push((n, out.len()));
            out.extend(format!("{n} 0 obj\n").bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        };
        obj(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
        obj(&mut out, 2, b"<< /Type /Pages /Kids [3 0 R] >>");
        let length = if how.length_by_ref {
            "600 0 R".to_string()
        } else {
            packed.len().to_string()
        };
        let mut stream = format!(
            "<< /Type /ObjStm /N {} /First {} /Filter /FlateDecode /Length {length} >>\nstream\n",
            bodies.len(),
            head.len()
        )
        .into_bytes();
        stream.extend(&packed);
        stream.extend(b"\nendstream");
        obj(&mut out, 500, &stream);
        if how.length_by_ref {
            obj(&mut out, 600, packed.len().to_string().as_bytes());
        }
        let xref_at = out.len();
        // Type, then four bytes, then two: an object written at an offset,
        // or one at an index of object stream 500.
        let mut rows = Vec::new();
        let mut index = String::new();
        for (n, o) in &at {
            rows.push(1u8);
            rows.extend((*o as u32).to_be_bytes());
            rows.extend([0, 0]);
            index.push_str(&format!("{n} 1 "));
        }
        rows.push(1);
        rows.extend((xref_at as u32).to_be_bytes());
        rows.extend([0, 0]);
        index.push_str("501 1 ");
        for i in 0..bodies.len() {
            rows.push(2);
            rows.extend(500u32.to_be_bytes());
            rows.extend((i as u16).to_be_bytes());
        }
        index.push_str(&format!("3 {}", bodies.len()));
        let rows = zlib(&rows);
        out.extend(
            format!(
                "501 0 obj\n<< /Type /XRef /Size 601 /W [1 4 2] /Index [{index}] /Root 1 0 R \
                 /Filter /FlateDecode /Length {} >>\nstream\n",
                rows.len()
            )
            .bytes(),
        );
        out.extend(&rows);
        out.extend(b"\nendstream\nendobj\n");
        if how.hybrid {
            let table = out.len();
            out.extend(b"xref\n0 1\n0000000000 65535 f \n");
            for (n, o) in &at {
                out.extend(format!("{n} 1\n{o:010} 00000 n \n").bytes());
            }
            out.extend(
                format!(
                    "trailer\n<< /Size 601 /Root 1 0 R /XRefStm {xref_at} >>\nstartxref\n{table}\n%%EOF\n"
                )
                .bytes(),
            );
        } else {
            out.extend(format!("startxref\n{xref_at}\n%%EOF\n").bytes());
        }
        out
    }

    /// A chain of `count` `/Pages` nodes, objects 3 onward, each naming the
    /// next, over a page.
    fn chain_bodies(count: usize) -> Vec<String> {
        let mut bodies: Vec<String> = (0..count)
            .map(|i| format!("<< /Type /Pages /Kids [{} 0 R] >>", 4 + i))
            .collect();
        bodies.push("<< /Type /Page /MediaBox [0 0 612 792] >>".into());
        bodies
    }

    #[test]
    fn an_object_stream_s_objects_go_by_the_index_a_cross_reference_stream_gives() {
        // The header numbers each object 900,000 and up: the parser goes
        // by the index, and walks the chain all the same.
        let bogus = |i: usize| 900_000 + i;
        for how in [
            Indexed::default(),
            Indexed {
                hybrid: true,
                ..Indexed::default()
            },
        ] {
            let file = indexed(&chain_bodies(40), bogus, how);
            assert_eq!(depth(&file), 41);
            assert_eq!(open(file).unwrap().pages().len(), 1);
            let e = refused(indexed(&chain_bodies(MAX_PAGE_TREE_DEPTH + 10), bogus, how));
            assert!(e.contains("nests more than 256 deep"), "{e}");
        }
        // The same objects numbered by their header too, as a table
        // rebuilt reads them, are not counted twice.
        let file = indexed(&chain_bodies(40), |i| 3 + i, Indexed::default());
        assert_eq!(
            measure(&file).unwrap(),
            Shape {
                depth: 41,
                pages: 1
            }
        );
        // A page named many times, through the index: a million pages.
        let fan = |kid: usize| {
            let kids = vec![format!("{kid} 0 R"); 1000].join(" ");
            format!("<< /Type /Pages /Kids [{kids}] >>")
        };
        let bodies = [fan(4), fan(5), "<< /Type /Page >>".into()];
        let e = refused(indexed(&bodies, bogus, Indexed::default()));
        assert!(e.contains("lists more than 250000 pages"), "{e}");
    }

    #[test]
    fn an_object_stream_s_length_given_by_reference_is_resolved() {
        // Stored, its first object holds `endstream`, where a stream read
        // without its length would be cut.
        let mut bodies = chain_bodies(MAX_PAGE_TREE_DEPTH + 10);
        bodies[0] = "<< /Type /Pages /Kids [4 0 R] /S (endstream) >>".into();
        let how = Indexed {
            length_by_ref: true,
            stored: true,
            ..Indexed::default()
        };
        let file = indexed(&bodies, |i| 3 + i, how);
        assert!(memchr::memmem::find_iter(&file, b"endstream").count() > 2);
        let e = refused(file);
        assert!(e.contains("nests more than 256 deep"), "{e}");
        // The parser reads it so: 40 levels down, it walks to the page.
        let mut bodies = chain_bodies(40);
        bodies[0] = "<< /Type /Pages /Kids [4 0 R] /S (endstream) >>".into();
        let file = indexed(&bodies, |i| 3 + i, how);
        assert_eq!(depth(&file), 41);
        assert_eq!(open(file).unwrap().pages().len(), 1);
    }

    #[test]
    fn cross_reference_streams_listing_too_many_objects_are_refused() {
        // One byte of type and one of offset an entry: as many as the
        // parser keeps in its table, past the bound.
        let entries = MAX_XREF_ENTRIES as usize + 1;
        let rows = zlib(&[1, 0].repeat(entries));
        let mut file = chain(3);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /XRef /Size {entries} /W [1 1 0] /Filter /FlateDecode /Length {} >>\nstream\n",
                rows.len()
            )
            .bytes(),
        );
        file.extend(&rows);
        file.extend(b"\nendstream\nendobj\n");
        assert!(file.len() < 64 << 10);
        let e = refused(file);
        assert!(e.contains("list more than"), "{e}");
        // At the bound, measured.
        let rows = zlib(&[1, 0].repeat(entries - 1));
        let mut file = chain(3);
        file.extend(
            format!(
                "400 0 obj\n<< /Type /XRef /Size {} /W [1 1 0] /Filter /FlateDecode /Length {} >>\nstream\n",
                entries - 1,
                rows.len()
            )
            .bytes(),
        );
        file.extend(&rows);
        file.extend(b"\nendstream\nendobj\n");
        assert_eq!(depth(&file), 3);
        assert_eq!(xref_number(&[1, 2]), Some(258));
        assert_eq!(xref_number(&[0, 0, 0, 0, 0, 0, 1, 0]), Some(256));
        assert_eq!(xref_number(&[1; 5]), None);
    }
}
