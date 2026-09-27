//! Meeting transcripts and captions to DoCO-typed document elements.
//!
//! WebVTT is what Zoom and Teams export a meeting's transcript as, and what
//! most notetakers export too; SubRip (`.srt`) is the same idea from video
//! subtitling, without the header or the voice tags. Both cut speech into cues
//! of a few seconds, each under a timing line, and both are read here into
//! what was said: one `doco:Paragraph` per speaker turn, its text
//! `"<speaker>: <words>"`, with who spoke and when carried as a [`Turn`].
//!
//! The file declares the timing exactly and usually the speaker. It declares
//! nothing about turns: a caption tool cuts every few seconds, so a sentence
//! runs across cues and a turn across many. Turns are the one thing this
//! reader decides. A turn is a run of cues from one speaker with no pause
//! longer than [`PAUSE_MS`] in it, and a long one ends at a sentence once it
//! has run [`LONG_TURN_MS`].
//!
//! Like DOCX there is no geometry and nothing to escalate: `bbox` is `None`
//! and `page` is 0 throughout.

use fluree_doc_model::{Element, Turn};
use std::collections::HashMap;

/// A silence longer than this ends a turn, even when the same speaker goes
/// on. Captions that name no speaker, as lecture and video subtitles do not,
/// would otherwise be one paragraph for the whole recording.
pub const PAUSE_MS: u64 = 2_000;

/// A turn that has run this long ends at the next cue that closes a
/// sentence. A monologue as one element gives every claim in it the same
/// start time, which locates none of them.
pub const LONG_TURN_MS: u64 = 60_000;

/// A turn that has run this long ends at the next cue, sentence or not:
/// machine captions often carry no punctuation to end one at.
pub const MAX_TURN_MS: u64 = 120_000;

/// The two caption formats this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    WebVtt,
    Srt,
}

impl Format {
    /// The format of a file by its content.
    ///
    /// No file type names a transcript reliably: `.vtt` has no registered
    /// type on most systems and arrives as `application/octet-stream`. The
    /// content does. WebVTT opens with `WEBVTT` and then a space, a tab, a
    /// line break or nothing. SubRip has no header, so it is recognised by
    /// its first cue: a number alone on a line, then a timing line.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        let head = decode(&bytes[..bytes.len().min(1024)]);
        if is_vtt(&head) {
            return Some(Self::WebVtt);
        }
        let mut lines = head.lines().map(str::trim).skip_while(|l| l.is_empty());
        let index = lines.next()?;
        let timing = lines.next()?;
        (is_digits(index) && index.len() <= 9 && parse_timing(timing).is_some())
            .then_some(Self::Srt)
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::WebVtt => "text/vtt",
            Self::Srt => "application/x-subrip",
        }
    }

    /// The `provenance` and `evidence` every element from this format carries.
    fn tag(self) -> &'static str {
        match self {
            Self::WebVtt => "vtt",
            Self::Srt => "srt",
        }
    }
}

#[derive(Debug)]
pub enum TranscriptError {
    /// `parse_vtt` was given text that does not open with `WEBVTT`.
    NoHeader,
    /// Nothing in the file is a caption cue.
    NoCues,
}

impl std::fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHeader => write!(f, "not a WebVTT file: it does not open with WEBVTT"),
            Self::NoCues => write!(f, "no caption cues: not a WebVTT or SubRip file"),
        }
    }
}

impl std::error::Error for TranscriptError {}

/// Parse a transcript file's bytes, WebVTT or SubRip, into document elements
/// in the order they were spoken.
///
/// A WebVTT file with no cues is not an error: it yields no elements, which
/// is what it holds. SubRip has no header to tell an empty file from some
/// other text, so a file with no cues at all is [`TranscriptError::NoCues`].
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, TranscriptError> {
    let text = decode(bytes);
    if is_vtt(&text) {
        parse_vtt(&text)
    } else {
        parse_srt(&text)
    }
}

/// Parse WebVTT text.
pub fn parse_vtt(text: &str) -> Result<Vec<Element>, TranscriptError> {
    let text = normalise(text);
    if !is_vtt(&text) {
        return Err(TranscriptError::NoHeader);
    }
    let blocks = blocks(&text);
    let mut notes: Vec<String> = Vec::new();
    let mut cues: Vec<Cue> = Vec::new();
    // The first block is the header, `WEBVTT` and any header lines under it.
    // A cue written straight under it, with no blank line, is still a cue.
    if let Some(at) = blocks[0].iter().position(|l| parse_timing(l).is_some()) {
        read_block(&blocks[0][at..], &mut cues);
    }
    for block in &blocks[1..] {
        let first = block[0];
        if keyword(first, "NOTE") {
            // Before the first cue a note describes the recording, and it is
            // where notetakers put the meeting's title and date: the only
            // place the date appears. Among the cues a note is about a cue
            // (a confidence score, an editor's remark) and is not speech.
            if cues.is_empty() {
                let note = note_text(block);
                if !note.is_empty() {
                    notes.push(note);
                }
            }
            continue;
        }
        if keyword(first, "STYLE") || keyword(first, "REGION") {
            continue;
        }
        read_block(block, &mut cues);
    }
    Ok(assemble(notes, cues, Format::WebVtt))
}

/// Parse SubRip text.
pub fn parse_srt(text: &str) -> Result<Vec<Element>, TranscriptError> {
    let text = normalise(text);
    let mut cues: Vec<Cue> = Vec::new();
    for block in blocks(&text) {
        read_block(&block, &mut cues);
    }
    if cues.is_empty() {
        return Err(TranscriptError::NoCues);
    }
    Ok(assemble(Vec::new(), cues, Format::Srt))
}

/// A cue as the file states it: its timing and its payload, markup and all.
struct Cue {
    start: u64,
    end: u64,
    payload: String,
}

/// What one voice said within a cue. A cue is usually one piece; a cue that
/// tags two voices is two, sharing its timing.
struct Piece {
    start: u64,
    end: u64,
    speaker: Option<String>,
    /// The speaker came from a voice tag rather than a label in the text.
    voiced: bool,
    lines: Vec<String>,
}

/// Read the cues in one blank-line-separated block into `cues`.
fn read_block(block: &[&str], cues: &mut Vec<Cue>) {
    let timings: Vec<(usize, (u64, u64))> = block
        .iter()
        .enumerate()
        .filter_map(|(i, l)| parse_timing(l).map(|t| (i, t)))
        .collect();
    if timings.is_empty() {
        // Text with no timing line, following a cue, is the rest of that
        // cue split off by a stray blank line. Dropping it would drop speech.
        if let Some(prev) = cues.last_mut() {
            for l in block {
                prev.payload.push('\n');
                prev.payload.push_str(l);
            }
        }
        return;
    }
    // Whatever precedes the first timing line is the cue's identifier.
    for (k, &(at, (start, end))) in timings.iter().enumerate() {
        let mut stop = timings.get(k + 1).map_or(block.len(), |&(next, _)| next);
        // Two cues run together without a blank line: the line above the
        // second timing is its identifier when it looks like one.
        if stop < block.len() && stop > at + 1 && is_identifier(block[stop - 1]) {
            stop -= 1;
        }
        cues.push(Cue {
            start,
            end,
            payload: block[at + 1..stop].join("\n"),
        });
    }
}

/// Turn the file's cues into elements: preamble notes first, then one
/// paragraph per speaker turn.
fn assemble(notes: Vec<String>, cues: Vec<Cue>, format: Format) -> Vec<Element> {
    let pieces = pieces(&cues);
    let mut out: Vec<Element> = notes
        .into_iter()
        .map(|n| element(n, None, format))
        .collect();
    for (turn, text) in turns(pieces) {
        out.push(element(text, Some(turn), format));
    }
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    out
}

/// Each cue's speech as pieces, with speakers resolved and rolling-caption
/// repeats removed.
fn pieces(cues: &[Cue]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    // The last line of the previous piece as the file has it, for the
    // repeat check below.
    let mut previous: Option<String> = None;
    for cue in cues {
        for said in voices(&cue.payload) {
            let mut lines = said.lines;
            let last = lines.last().cloned();
            // Rolling captions (auto-generated video subtitles) scroll: each
            // cue repeats the line before it above the new one, and a cue a
            // few milliseconds long holds the repeated line alone. Read as
            // is, every line of speech appears two or three times. Only the
            // scrolling shape qualifies, so a speaker who says "Yes." twice
            // in two ordinary cues is heard twice.
            let scrolls = lines.len() > 1 || cue.end.saturating_sub(cue.start) < 100;
            if scrolls && previous.is_some() && lines.first() == previous.as_ref() {
                lines.remove(0);
            }
            previous = last;
            out.push(Piece {
                start: cue.start,
                end: cue.end,
                voiced: said.voice.is_some(),
                speaker: said.voice,
                lines,
            });
        }
    }
    resolve_labels(&mut out);
    out
}

/// Take `Name: ` prefixes that label a speaker out of the text and into
/// `speaker`, for pieces no voice tag named.
///
/// Zoom and most notetakers write the speaker into the cue text. What looks
/// like a label is accepted when the same label starts at least two cues
/// (people speak more than once, and an interjection with a colon in it
/// rarely recurs word for word), when it is a numbered label like
/// `Speaker 2`, or when it is two to four capitalised words, which is the
/// shape of a name heard once.
fn resolve_labels(pieces: &mut [Piece]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for p in pieces.iter().filter(|p| p.speaker.is_none()) {
        if let Some((name, _)) = p.lines.first().and_then(|l| split_label(l)) {
            *seen.entry(name.to_string()).or_default() += 1;
        }
    }
    for p in pieces.iter_mut().filter(|p| p.speaker.is_none()) {
        let Some((name, rest)) = p.lines.first().and_then(|l| split_label(l)) else {
            continue;
        };
        if !accepted(name, seen.get(name).copied().unwrap_or(0)) {
            continue;
        }
        let (name, rest) = (name.to_string(), rest.to_string());
        p.speaker = Some(name);
        if rest.is_empty() {
            p.lines.remove(0);
        } else {
            p.lines[0] = rest;
        }
    }
}

/// Group pieces into turns: `(turn, text)` in order.
fn turns(pieces: Vec<Piece>) -> Vec<(Turn, String)> {
    // A file that tags voices tags every cue whose speaker it knows, so an
    // untagged cue there has no known speaker. A file that labels speakers
    // in the text may label only where the speaker changes, so there an
    // unlabelled cue continues whoever was speaking.
    let inherit = !pieces.iter().any(|p| p.voiced);

    let mut out = Vec::new();
    let mut cur: Option<Open> = None;
    for p in pieces {
        let words = p.lines.join(" ");
        let Some(c) = cur.as_mut() else {
            cur = Some(Open::new(p.speaker, p.start, p.end, words));
            continue;
        };
        let gap = p.start.saturating_sub(c.end);
        let speaker = match p.speaker {
            Some(s) => Some(s),
            None if inherit && gap <= PAUSE_MS => c.speaker.clone(),
            None => None,
        };
        let ran = c.end.saturating_sub(c.start);
        let continues = speaker == c.speaker
            && gap <= PAUSE_MS
            && ran < MAX_TURN_MS
            && !(ran >= LONG_TURN_MS && ends_sentence(&c.words));
        // A piece with nothing new in it, a rolling caption's repeat, belongs
        // to the turn it repeats and never opens one.
        if continues || (words.is_empty() && speaker.is_none()) {
            c.push(&words, p.end);
            continue;
        }
        out.extend(cur.take().and_then(Open::finish));
        cur = Some(Open::new(speaker, p.start, p.end, words));
    }
    out.extend(cur.and_then(Open::finish));
    out
}

/// A turn being built.
struct Open {
    speaker: Option<String>,
    start: u64,
    end: u64,
    words: String,
}

impl Open {
    fn new(speaker: Option<String>, start: u64, end: u64, words: String) -> Self {
        Open {
            speaker,
            start,
            end,
            words,
        }
    }

    fn push(&mut self, words: &str, end: u64) {
        if !words.is_empty() {
            if !self.words.is_empty() {
                self.words.push(' ');
            }
            self.words.push_str(words);
        }
        self.end = self.end.max(end);
    }

    /// The finished turn, or nothing when no words were said in it: a cue
    /// holding only a speaker label opens a turn and fills none.
    fn finish(self) -> Option<(Turn, String)> {
        if self.words.is_empty() {
            return None;
        }
        let text = match &self.speaker {
            Some(s) => format!("{s}: {}", self.words),
            None => self.words,
        };
        Some((
            Turn {
                speaker: self.speaker,
                start_ms: self.start,
                end_ms: self.end,
            },
            text,
        ))
    }
}

fn element(text: String, turn: Option<Turn>, format: Format) -> Element {
    Element {
        id: String::new(),
        kind: "doco:Paragraph".into(),
        page: 0,
        bbox: None,
        text,
        level: None,
        cells: None,
        header_rows: None,
        sub_headers: None,
        merged_down: None,
        merged_left: None,
        figure: None,
        links: None,
        turn,
        provenance: format.tag(),
        evidence: format.tag(),
    }
}

/// What each voice in a cue said, as cleaned lines.
struct Said {
    voice: Option<String>,
    lines: Vec<String>,
}

/// Split a cue payload by voice and strip its markup.
///
/// `<v Ada Park>` opens a voice and `</v>` closes it; every other tag (class,
/// italic, bold, underline, ruby, language, an inline timestamp, SubRip's
/// `<font>`) styles the words and is not one of them.
fn voices(payload: &str) -> Vec<Said> {
    let mut out: Vec<Said> = Vec::new();
    let mut voice: Option<String> = None;
    let mut text = String::new();
    let flush = |out: &mut Vec<Said>, voice: &Option<String>, text: &mut String| {
        let lines = clean_lines(&std::mem::take(text));
        if !lines.is_empty() {
            out.push(Said {
                voice: voice.clone(),
                lines,
            });
        }
    };
    let mut rest = payload;
    while let Some(lt) = rest.find('<') {
        text.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        // A `<` that opens no tag is a stray character, kept as text.
        let opens = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '/');
        let Some(gt) = after.find('>').filter(|_| opens) else {
            text.push('<');
            rest = after;
            continue;
        };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        if let Some(name) = voice_open(tag) {
            flush(&mut out, &voice, &mut text);
            voice = name;
        } else if tag.trim() == "/v" {
            flush(&mut out, &voice, &mut text);
            voice = None;
        }
    }
    text.push_str(rest);
    flush(&mut out, &voice, &mut text);
    out
}

/// The name a voice tag opens, if `tag` is one: `v Ada Park` or
/// `v.loud Ada Park`, where the classes come before the name.
fn voice_open(tag: &str) -> Option<Option<String>> {
    let rest = tag.strip_prefix('v')?;
    if !(rest.is_empty() || rest.starts_with(['.', ' ', '\t'])) {
        return None;
    }
    Some(
        rest.split_once([' ', '\t'])
            .map(|(_, name)| collapse(&decode_entities(name)))
            .filter(|n| !n.is_empty()),
    )
}

/// Markup-free text as non-empty lines, whitespace collapsed.
fn clean_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| collapse(&decode_entities(&strip_overrides(l))))
        .filter(|l| !l.is_empty())
        .collect()
}

/// Remove `{\an8}`-style overrides, which SubRip files converted from ASS
/// subtitles carry to position a line.
fn strip_overrides(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("{\\") {
        out.push_str(&rest[..i]);
        match rest[i..].find('}') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decode character references in one pass, so `&amp;lt;` stays `&lt;`.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after
            .find(';')
            .filter(|&i| i <= 10)
            .and_then(|i| entity(&after[..i]).map(|c| (c, i)));
        match decoded {
            Some((c, i)) => {
                out.extend(c);
                rest = &after[i + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A character reference's replacement: `Some(None)` for one that decodes
/// to nothing, `None` for a name this is not.
fn entity(name: &str) -> Option<Option<char>> {
    Some(match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        // Direction marks steer rendering and carry no words.
        "lrm" | "rlm" => None,
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            Some(char::from_u32(code)?)
        }
    })
}

/// A leading `Name: ` label, split from the words after it.
fn split_label(line: &str) -> Option<(&str, &str)> {
    let (at, colon) = line.char_indices().find(|(_, c)| matches!(c, ':' | '：'))?;
    let name = line[..at].trim_end();
    let rest = &line[at + colon.len_utf8()..];
    // `10:30`, `https://`: a colon inside a word labels nothing. The
    // full-width colon is followed by no space in the scripts that use it.
    if colon == ':' && !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    name_shaped(name).then(|| (name, rest.trim()))
}

/// Could this be a name? One to five words, starting with a letter that is
/// not lower case, and nothing a sentence has that a name does not. Meeting
/// display names carry more than a name, as in `Ada Park (she/her)` or
/// `Ada Park | Northwind`, so those marks are allowed.
fn name_shaped(name: &str) -> bool {
    let words = name.split_whitespace().count();
    (1..=5).contains(&words)
        && name.chars().count() <= 40
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() && !c.is_lowercase())
        && name.chars().all(|c| {
            c.is_alphanumeric()
                || c.is_whitespace()
                || matches!(
                    c,
                    '.' | '-' | '_' | '\'' | '’' | '(' | ')' | '/' | '|' | '&' | ',' | '@'
                )
        })
}

fn accepted(name: &str, seen: usize) -> bool {
    seen >= 2 || numbered(name) || full_name(name)
}

/// Two to four capitalised words of letters: a name, even when it is heard
/// only once. Stricter than [`name_shaped`], because a label seen once has
/// nothing else vouching for it, and `Well, Tom` is two capitalised words.
fn full_name(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    (2..=4).contains(&words.len())
        && words.iter().all(|w| {
            w.chars().next().is_some_and(char::is_uppercase)
                && w.chars()
                    .all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’'))
        })
}

/// `Speaker 2`, `SPEAKER_01`, `Speaker B`: the label a transcriber gives a
/// voice it could not name.
fn numbered(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(rest) = lower.strip_prefix("speaker") else {
        return false;
    };
    let id = rest.trim_start_matches([' ', '_']);
    id.len() < rest.len()
        && !id.is_empty()
        && (is_digits(id) || (id.len() == 1 && id.chars().all(|c| c.is_ascii_alphabetic())))
}

fn ends_sentence(words: &str) -> bool {
    words
        .trim_end()
        .trim_end_matches(['"', '\'', '”', '’', ')', ']'])
        .ends_with(['.', '?', '!', '…', '。', '？', '！'])
}

/// `start --> end`, with anything after the end time (WebVTT cue settings,
/// SubRip coordinates) ignored.
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let (a, b) = line.split_once("-->")?;
    let start = timestamp(a.trim())?;
    let end = timestamp(b.split_whitespace().next()?)?;
    Some((start, end.max(start)))
}

/// `hh:mm:ss.ttt` or `mm:ss.ttt` in milliseconds. SubRip's comma is accepted
/// in WebVTT and the fraction may be short or missing, because files written
/// by hand and by lesser tools are.
fn timestamp(s: &str) -> Option<u64> {
    let (clock, frac) = s.rsplit_once(['.', ',']).unwrap_or((s, ""));
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| !is_digits(p) || p.len() > 6) {
        return None;
    }
    let n: Vec<u64> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let (h, m, sec) = match n[..] {
        [m, sec] => (0, m, sec),
        [h, m, sec] => {
            if m >= 60 {
                return None;
            }
            (h, m, sec)
        }
        _ => return None,
    };
    if sec >= 60 {
        return None;
    }
    let ms = if frac.is_empty() {
        0
    } else {
        if !is_digits(frac) || frac.len() > 9 {
            return None;
        }
        let padded: String = frac.chars().chain(std::iter::repeat('0')).take(3).collect();
        padded.parse().ok()?
    };
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

/// Blank-line-separated blocks of lines. A line of only whitespace counts
/// as blank.
fn blocks(text: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(line.trim_end());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn note_text(block: &[&str]) -> String {
    std::iter::once(block[0]["NOTE".len()..].trim())
        .chain(block[1..].iter().map(|l| l.trim()))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does `line` open with `word` as a whole word?
fn keyword(line: &str, word: &str) -> bool {
    line.strip_prefix(word)
        .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']))
}

/// A cue identifier: one token, no spaces (`12`, `0f9a…/12-0`).
fn is_identifier(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.contains(char::is_whitespace) && parse_timing(t).is_none()
}

fn is_vtt(text: &str) -> bool {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .strip_prefix("WEBVTT")
        .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t', '\n', '\r']))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Line endings made `\n` and a byte-order mark removed.
fn normalise(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// Bytes to text. WebVTT is UTF-8 by definition; SubRip is whatever the
/// subtitler's machine used, so a byte-order mark is honoured and bytes that
/// are not UTF-8 are read as Windows-1252, the usual alternative.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return utf16(rest, u16::from_be_bytes);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

fn cp1252(b: u8) -> char {
    // 0x80-0x9F are where Windows-1252 departs from Latin-1; the five
    // unassigned positions map to the control characters Latin-1 has there.
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match b {
        0x80..=0x9F => char::from_u32(u32::from(HIGH[usize::from(b - 0x80)])).unwrap_or('\u{FFFD}'),
        _ => char::from(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(speaker, words, start_ms, end_ms)` for each turn, the speaker label
    /// checked against the text and taken off it.
    fn turns_of(els: &[Element]) -> Vec<(Option<String>, String, u64, u64)> {
        els.iter()
            .filter_map(|e| {
                let t = e.turn.as_ref()?;
                let words = match &t.speaker {
                    Some(s) => e
                        .text
                        .strip_prefix(&format!("{s}: "))
                        .expect("the label opens the text")
                        .to_string(),
                    None => e.text.clone(),
                };
                Some((t.speaker.clone(), words, t.start_ms, t.end_ms))
            })
            .collect()
    }

    fn said(els: &[Element]) -> Vec<(Option<&str>, &str)> {
        els.iter()
            .map(|e| {
                let speaker = e.turn.as_ref().and_then(|t| t.speaker.as_deref());
                let words = speaker.map_or(e.text.as_str(), |s| &e.text[s.len() + 2..]);
                (speaker, words)
            })
            .collect()
    }

    #[test]
    fn a_teams_transcript_reads_as_voice_tagged_turns() {
        let src = "\u{feff}WEBVTT\r\n\r\n\
            7c1e/12-0\r\n00:00:03.120 --> 00:00:09.480\r\n<v Ada Park>Thanks, <i>all</i> &amp; welcome. Before we start, I want</v>\r\n\r\n\
            7c1e/13-0\r\n00:00:09.600 --> 00:00:12.000\r\n<v Ada Park>to say this is my last review.</v>\r\n\r\n\
            7c1e/14-0\r\n00:00:12.900 --> 00:00:14.000\r\n<v.loud Ben Ortiz>Morning.</v>\r\n";
        let els = parse(src.as_bytes()).unwrap();
        assert_eq!(
            turns_of(&els),
            vec![
                (
                    Some("Ada Park".into()),
                    "Thanks, all & welcome. Before we start, I want to say this is my last review."
                        .into(),
                    3120,
                    12000
                ),
                (Some("Ben Ortiz".into()), "Morning.".into(), 12900, 14000),
            ]
        );
        let e = &els[0];
        assert_eq!(e.kind, "doco:Paragraph");
        assert_eq!((e.page, e.bbox, e.evidence), (0, None, "vtt"));
        assert_eq!(e.id, "elem-00001");
    }

    #[test]
    fn a_zoom_transcript_labels_speakers_in_the_text() {
        let src = "WEBVTT\n\n\
            1\n00:02.400 --> 00:04.708 line:90% align:start\nKai Moreno: Hi, hi. Can you hear\n\n\
            2\n00:04.858 --> 00:05.100\nKai Moreno: me?\n\n\
            3\n00:05.458 --> 00:10.842\nLena Holt: We can hear you, Kai.\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![
                (Some("Kai Moreno"), "Hi, hi. Can you hear me?"),
                (Some("Lena Holt"), "We can hear you, Kai."),
            ]
        );
    }

    #[test]
    fn a_notetakers_preamble_note_is_kept_and_an_interjection_is_not_a_speaker() {
        let src = "WEBVTT\n\n\
            NOTE\nQuarterly review\nRecorded 3 March 2026.\n\n\
            1\n00:00:02.400 --> 00:00:04.708\nKai Moreno: Hi. Can you hear me?\n\n\
            2\n00:00:05.458 --> 00:00:10.842\nSpeaker 2: We can hear you.\n\n\
            3\n00:00:11.000 --> 00:00:12.000\nKai Moreno: Great.\n\n\
            4\n00:01:02.000 --> 00:01:04.000\nOkay: so, where were we\n";
        let els = parse_vtt(src).unwrap();
        // The note is where the title and date live, so it stays, lines
        // intact, and is not speech.
        assert_eq!(els[0].text, "Quarterly review\nRecorded 3 March 2026.");
        assert!(els[0].turn.is_none());
        assert_eq!(
            said(&els[1..]),
            vec![
                (Some("Kai Moreno"), "Hi. Can you hear me?"),
                // Heard once, but a numbered label is a speaker by its shape.
                (Some("Speaker 2"), "We can hear you."),
                (Some("Kai Moreno"), "Great."),
                // Once, one word, and after a pause: words, not a label.
                (None, "Okay: so, where were we"),
            ]
        );
    }

    #[test]
    fn a_label_is_a_speaker_when_it_recurs_or_is_shaped_like_a_full_name() {
        let src = "WEBVTT\n\n\
            00:01.000 --> 00:02.000\nKim: First.\n\n\
            00:05.000 --> 00:06.000\nJo Tanaka: Once only.\n\n\
            00:09.000 --> 00:10.000\nKim: Second.\n\n\
            00:13.000 --> 00:14.000\nNote: one word, once.\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![
                (Some("Kim"), "First."),
                (Some("Jo Tanaka"), "Once only."),
                (Some("Kim"), "Second."),
                (None, "Note: one word, once."),
            ]
        );
    }

    #[test]
    fn a_display_name_with_pronouns_or_a_company_is_a_speaker() {
        let src = "WEBVTT\n\n\
            00:01.000 --> 00:02.000\nAda Park (she/her): Morning.\n\n\
            00:02.100 --> 00:03.000\nBen Ortiz | Northwind: Hi Ada.\n\n\
            00:03.100 --> 00:04.000\nAda Park (she/her): Shall we start?\n\n\
            00:04.100 --> 00:05.000\nBen Ortiz | Northwind: Yes.\n\n\
            00:09.000 --> 00:10.000\nWell, Tom: that is once and not a name.\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![
                (Some("Ada Park (she/her)"), "Morning."),
                (Some("Ben Ortiz | Northwind"), "Hi Ada."),
                (Some("Ada Park (she/her)"), "Shall we start?"),
                (Some("Ben Ortiz | Northwind"), "Yes."),
                (None, "Well, Tom: that is once and not a name."),
            ]
        );
    }

    #[test]
    fn a_label_on_the_first_cue_carries_through_the_turn_until_a_pause() {
        let src = "WEBVTT\n\n\
            00:01.000 --> 00:03.000\nKai Moreno: The pilot starts in May,\n\n\
            00:03.100 --> 00:05.000\nand the budget is approved.\n\n\
            00:05.100 --> 00:06.000\nLena Holt: Good.\n\n\
            00:06.100 --> 00:07.000\nKai Moreno: One more thing.\n\n\
            00:12.000 --> 00:13.000\nApplause.\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![
                (
                    Some("Kai Moreno"),
                    "The pilot starts in May, and the budget is approved."
                ),
                (Some("Lena Holt"), "Good."),
                (Some("Kai Moreno"), "One more thing."),
                (None, "Applause."),
            ]
        );
    }

    #[test]
    fn in_a_voice_tagged_file_an_untagged_cue_has_no_speaker() {
        let src = "WEBVTT\n\n\
            00:01.000 --> 00:02.000\n<v Ada Park>Hello.</v>\n\n\
            00:02.100 --> 00:03.000\nHello from the room.\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![(Some("Ada Park"), "Hello."), (None, "Hello from the room.")]
        );
    }

    #[test]
    fn two_voices_in_one_cue_are_two_turns() {
        let src =
            "WEBVTT\n\n00:01.000 --> 00:03.000\n<v Ada Park>Ready?</v> <v Ben Ortiz>Ready.</v>\n";
        let els = parse_vtt(src).unwrap();
        assert_eq!(
            said(&els),
            vec![(Some("Ada Park"), "Ready?"), (Some("Ben Ortiz"), "Ready.")]
        );
        assert_eq!(els[1].turn.as_ref().unwrap().start_ms, 1000);
    }

    #[test]
    fn captions_with_no_speakers_break_at_pauses() {
        let src = "WEBVTT\n\n\
            00:00.000 --> 00:02.000\nToday we look at\n\n\
            00:02.000 --> 00:04.000\nthree kinds of rock.\n\n\
            00:07.000 --> 00:09.000\nThe first is igneous.\n";
        let els = parse_vtt(src).unwrap();
        assert_eq!(
            turns_of(&els),
            vec![
                (
                    None,
                    "Today we look at three kinds of rock.".into(),
                    0,
                    4000
                ),
                (None, "The first is igneous.".into(), 7000, 9000),
            ]
        );
    }

    #[test]
    fn a_long_turn_ends_at_a_sentence_and_a_longer_one_ends_regardless() {
        // Contiguous cues from one speaker, ten seconds each.
        let mut src = String::from("WEBVTT\n\n");
        for i in 0..8u64 {
            src.push_str(&format!(
                "00:{:02}:{:02}.000 --> 00:{:02}:{:02}.000\nAda Park: Point {i}.\n\n",
                i * 10 / 60,
                i * 10 % 60,
                (i + 1) * 10 / 60,
                (i + 1) * 10 % 60
            ));
        }
        let t = turns_of(&parse_vtt(&src).unwrap());
        // At 60 s the turn has closed a sentence, so the next cue starts a
        // new one: the same speaker, twice.
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].2, t[0].3, t[1].2), (0, 60_000, 60_000));

        // Unpunctuated machine captions have no sentence to end at.
        let mut src = String::from("WEBVTT\n\n");
        for i in 0..15u64 {
            src.push_str(&format!(
                "00:{:02}:{:02}.000 --> 00:{:02}:{:02}.000\nand so on\n\n",
                i * 10 / 60,
                i * 10 % 60,
                (i + 1) * 10 / 60,
                (i + 1) * 10 % 60
            ));
        }
        let t = turns_of(&parse_vtt(&src).unwrap());
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].3, MAX_TURN_MS);
    }

    #[test]
    fn rolling_captions_are_read_once() {
        // The scrolling shape of auto-generated video subtitles: each cue
        // repeats the line above the new one, and a flash cue holds the
        // repeat alone.
        let src = "WEBVTT\nKind: captions\nLanguage: en\n\n\
            00:00:00.000 --> 00:00:02.000 align:start position:0%\n \nwelcome<00:00:00.500><c> back</c><00:00:01.000><c> everyone</c>\n\n\
            00:00:02.000 --> 00:00:02.010 align:start position:0%\nwelcome back everyone\n \n\n\
            00:00:02.010 --> 00:00:04.000 align:start position:0%\nwelcome back everyone\ntoday<00:00:02.500><c> is</c><00:00:03.000><c> about</c><00:00:03.500><c> soil</c>\n\n\
            00:00:04.000 --> 00:00:04.010 align:start position:0%\ntoday is about soil\n \n";
        assert_eq!(
            turns_of(&parse_vtt(src).unwrap()),
            vec![(
                None,
                "welcome back everyone today is about soil".into(),
                0,
                4010
            )]
        );
    }

    #[test]
    fn a_word_said_twice_in_ordinary_cues_is_heard_twice() {
        let src = "WEBVTT\n\n\
            00:01.000 --> 00:02.000\n<v Ada Park>Yes.</v>\n\n\
            00:02.100 --> 00:03.000\n<v Ada Park>Yes.</v>\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![(Some("Ada Park"), "Yes. Yes.")]
        );
    }

    #[test]
    fn notes_among_cues_styles_and_regions_are_not_speech() {
        let src = "WEBVTT\n\n\
            STYLE\n::cue { color: yellow }\n\n\
            REGION\nid:fred width:40%\n\n\
            00:01.000 --> 00:02.000\n<v Ada Park>The contract</v>\n\n\
            NOTE Confidence: 0.91\n\n\
            00:02.100 --> 00:03.000\n<v Ada Park>renews in June.</v>\n";
        let els = parse_vtt(src).unwrap();
        assert_eq!(
            said(&els),
            vec![(Some("Ada Park"), "The contract renews in June.")]
        );
    }

    #[test]
    fn an_empty_transcript_is_empty_and_other_text_is_not_a_transcript() {
        assert!(parse(b"WEBVTT\n\n").unwrap().is_empty());
        assert!(parse(b"WEBVTT - nothing yet").unwrap().is_empty());
        assert!(matches!(
            parse_vtt("Hello there"),
            Err(TranscriptError::NoHeader)
        ));
        assert!(matches!(
            parse(b"Just some notes: nothing timed."),
            Err(TranscriptError::NoCues)
        ));
    }

    #[test]
    fn a_subrip_file_reads_like_a_transcript() {
        let src = "1\r\n00:00:01,000 --> 00:00:03,500\r\n{\\an8}<i>Kai Moreno: Where does</i>\r\nthe line go?\r\n\r\n\
            2\r\n00:00:03,600 --> 00:00:05,000\r\n<font color=\"#ffff00\">Lena Holt:</font> Down the hill.\r\n\r\n\
            3\r\n00:00:05,100 --> 00:00:06,000\r\nKai Moreno: Thanks.\r\n\r\n\
            4\r\n00:00:06,100 --> 00:00:07,000\r\nLena Holt: Any time.\r\n";
        let els = parse(src.as_bytes()).unwrap();
        assert_eq!(
            said(&els),
            vec![
                (Some("Kai Moreno"), "Where does the line go?"),
                (Some("Lena Holt"), "Down the hill."),
                (Some("Kai Moreno"), "Thanks."),
                (Some("Lena Holt"), "Any time."),
            ]
        );
        assert_eq!((els[0].provenance, els[0].evidence), ("srt", "srt"));
        assert_eq!(els[0].turn.as_ref().unwrap().start_ms, 1000);
    }

    #[test]
    fn subrip_cues_run_together_or_split_by_a_blank_line_still_read() {
        // No blank line between cues 1 and 2; a stray one inside cue 3.
        let src =
            "1\n00:00:01,000 --> 00:00:02,000\nOne.\n2\n00:00:02,100 --> 00:00:03,000\nTwo.\n\n\
            3\n00:00:03,100 --> 00:00:04,000\nThree,\n\nand four.\n";
        assert_eq!(
            said(&parse_srt(src).unwrap()),
            vec![(None, "One. Two. Three, and four.")]
        );
    }

    #[test]
    fn subrip_in_a_legacy_encoding_decodes() {
        let mut cp1252 = b"1\n00:00:01,000 --> 00:00:02,000\nCaf".to_vec();
        cp1252.extend([0xE9, b' ', 0x93, b'o', b'k', 0x94]);
        assert_eq!(said(&parse(&cp1252).unwrap()), vec![(None, "Café “ok”")]);

        let text = "1\n00:00:01,000 --> 00:00:02,000\nÜber alles\n";
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(said(&parse(&utf16).unwrap()), vec![(None, "Über alles")]);
    }

    #[test]
    fn formats_are_told_apart_by_content() {
        assert_eq!(Format::sniff(b"WEBVTT\n\n"), Some(Format::WebVtt));
        assert_eq!(Format::sniff(b"\xEF\xBB\xBFWEBVTT"), Some(Format::WebVtt));
        assert_eq!(Format::sniff(b"WEBVTT\tcaptions"), Some(Format::WebVtt));
        assert_eq!(Format::sniff(b"WEBVTTX not a transcript"), None);
        assert_eq!(
            Format::sniff(b"\n1\n00:00:01,000 --> 00:00:02,000\nHi\n"),
            Some(Format::Srt)
        );
        assert_eq!(Format::sniff(b"%PDF-1.7"), None);
        assert_eq!(Format::sniff(b"Meeting notes\n12:00 lunch"), None);
    }

    #[test]
    fn timestamps_read_in_every_form_the_formats_allow() {
        assert_eq!(timestamp("04:32.404"), Some(272_404));
        assert_eq!(timestamp("01:02:03.500"), Some(3_723_500));
        assert_eq!(timestamp("00:00:01,5"), Some(1_500));
        assert_eq!(timestamp("00:00:07"), Some(7_000));
        assert_eq!(timestamp("100:00:00.000"), Some(360_000_000));
        assert_eq!(timestamp("00:61.000"), None);
        assert_eq!(timestamp("12.5"), None);
        assert_eq!(timestamp("aa:bb.ccc"), None);
        assert_eq!(
            parse_timing("00:01.000 --> 00:02.000 line:90% align:start"),
            Some((1_000, 2_000))
        );
        assert_eq!(parse_timing("see --> there"), None);
    }

    #[test]
    fn references_decode_once_and_direction_marks_vanish() {
        assert_eq!(decode_entities("a &amp;lt; b"), "a &lt; b");
        assert_eq!(decode_entities("&lrm;left&rlm;"), "left");
        assert_eq!(decode_entities("&#233;t&#xE9;"), "été");
        assert_eq!(decode_entities("R&D; fish & chips"), "R&D; fish & chips");
    }

    #[test]
    fn a_stray_angle_bracket_is_text() {
        // Neither opens a tag: one is followed by a space, the other never
        // closes.
        let src = "WEBVTT\n\n00:01.000 --> 00:02.000\nx < y and 3 <4\n";
        assert_eq!(
            said(&parse_vtt(src).unwrap()),
            vec![(None, "x < y and 3 <4")]
        );
    }
}
