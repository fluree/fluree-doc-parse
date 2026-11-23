//! Rich Text Format (`.rtf`) to DoCO-typed document elements.
//!
//! RTF is text interleaved with control words (`\par`, `\b`, `\cell`) in
//! nested groups (`{…}`), some of which are destinations holding something
//! other than the text: the font table, the style sheet, a picture, a
//! footnote. It declares its structure, so the reader maps rather than
//! measures:
//!
//! * **Paragraphs** end at `\par`; `\line` is a line break, `\tab` a space,
//!   `\pard` resets what the paragraph is.
//! * **Headings**: a paragraph with an outline level (`\outlinelevelN`,
//!   directly or through its style, `\sN`), or in a style named `heading N`,
//!   is a heading at that level, numbered or not; a paragraph in the style
//!   named `Title` is the document's title.
//! * **Lists**: a paragraph in a list (`\lsN`) that is no heading is a list
//!   item; the numbers and bullets a list draws (`\listtext`, `\pntext`)
//!   are not text.
//! * **Tables**: paragraphs in a table (`\intbl`) fill its cells, `\cell`
//!   ends a cell and `\row` a row; nested tables are read flat. Where each
//!   row states its cells' right edges (`\cellxN`), the columns are the
//!   edges all rows state, so a cell as wide as two is one cell and the
//!   column it covers a continuation (`merged_left`), as is a cell merged
//!   into the one before it (`\clmrg`) or the one above (`\clvmrg`,
//!   `merged_down`).
//! * **Footnotes and endnotes** (`\footnote`, `\ftnalt`) are read after the
//!   body, each opening with its number, marked `evidence` `footnote` or
//!   `endnote`; **comments** (`\annotation`) last, opening with their
//!   author; the **page header and footer** (`\header`, `\footer` and their
//!   first-page and left/right variants) once each, before and after the
//!   body, marked `header` and `footer`.
//! * **Encodings**: `\ansicpg` names the code page bytes and `\'hh` escapes
//!   are in (Windows-1252 by default), and a font's character set
//!   (`\fcharsetN` in the font table) the code page of text set in it;
//!   `\uN` is a Unicode character (two of them a surrogate pair), followed
//!   by `\ucN` fallback characters that are skipped.
//! * **Pictures** (`\pict`) are each an empty `doco:Figure` where they
//!   stand; fields show their result (`\fldrslt`), not their code.
//! * **Skipped**: the colour, list and revision tables, the font table's
//!   and the style sheet's own text, document variables, shapes (their
//!   drawing data, and the text of a text box, `\shptxt`), embedded objects,
//!   and any destination marked ignorable (`\*`) that is not one of the
//!   above.
//!
//! **Bounds**: groups nest at most [`MAX_DEPTH`] deep; at most [`MAX_FLOWS`]
//! notes, comments, headers and footers are read; a row definition states
//! at most [`MAX_TABLE_CELLS`] cells; and what the document makes is spent
//! from one [`Budget`] before it is made: each paragraph, with its text, and
//! each table's cells, a row and the copy of its definitions at its `\row`,
//! a short row's padding to its table's width as the table is laid out. A
//! file past any of them fails as too large. `\binN` data is skipped, never past the end. What is read
//! is otherwise bounded by the file's own size.

use fluree_doc_model::budget::{Budget, TooLarge};
use fluree_doc_model::{DocumentInfo, Element};
use std::collections::{HashMap, HashSet};

/// The deepest nesting of groups read.
pub const MAX_DEPTH: usize = 1024;

/// The most notes, comments, headers and footers read: each is a flow of
/// its own, made as it opens, and a few bytes open one.
pub const MAX_FLOWS: usize = 100_000;

/// The most cells one row definition (`\trowd` … `\cellxN`) states.
pub const MAX_TABLE_CELLS: usize = 1 << 22;

#[derive(Debug, PartialEq, Eq)]
pub enum RtfError {
    /// The file does not open with `{\rtf`.
    NotRtf,
    /// Groups nested past [`MAX_DEPTH`].
    TooDeep,
    /// Past another bound, naming it.
    TooLarge(String),
}

impl std::fmt::Display for RtfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRtf => write!(f, "not an RTF document: it does not open with {{\\rtf"),
            Self::TooDeep => write!(f, "RTF groups nested more than {MAX_DEPTH} deep"),
            Self::TooLarge(e) => write!(f, "RTF document too large: {e}"),
        }
    }
}

impl std::error::Error for RtfError {}

/// Whether `bytes` is an RTF document: it opens with `{\rtf`, after any
/// whitespace.
pub fn is_rtf(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    bytes[start..].starts_with(b"{\\rtf")
}

/// Where a group's text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dest {
    /// A flow of paragraphs: the body, a header, a footnote.
    Flow(usize),
    /// Nowhere.
    Skip,
    /// The document's title, or its author (`\info`).
    Title,
    Author,
    /// A comment's author (`\atnauthor`).
    AtnAuthor,
    /// A style's definition in the style sheet.
    Style,
    /// The font table: each font's character set.
    Fonts,
}

/// A group's state.
#[derive(Debug, Clone, Copy)]
struct Group {
    dest: Dest,
    /// How many fallback characters follow a `\uN`.
    uc: usize,
    /// The font its text is set in (`\fN`), by number.
    font: Option<i32>,
}

/// What a row's definition says of one cell: its right edge, and whether
/// it continues the cell before it or the one above.
#[derive(Debug, Clone, Copy, Default)]
struct CellDef {
    right: i32,
    merged_left: bool,
    merged_down: bool,
}

/// A run of paragraphs: the body, a header or footer, a note, a comment.
#[derive(Debug, Default)]
struct Flow {
    evidence: &'static str,
    /// Opens its first paragraph: a note's number, a comment's author.
    label: String,
    para: String,
    /// The paragraph's own properties, reset by `\pard`.
    in_table: bool,
    level: Option<usize>,
    style: Option<i32>,
    list: bool,
    cell: Vec<String>,
    row: Vec<String>,
    /// Each row read, with its cells' definitions as of its `\row`.
    rows: Vec<(Vec<String>, Vec<CellDef>)>,
    /// The row definition being read (`\trowd` … `\cellxN`), and the cell
    /// it is defining.
    defs: Vec<CellDef>,
    def: CellDef,
    out: Vec<Element>,
}

/// What a style in the style sheet makes a paragraph.
#[derive(Debug, Default, Clone)]
struct Style {
    level: Option<usize>,
    name: String,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    encoding: &'static encoding_rs::Encoding,
    /// Bytes of text waiting to be decoded in the code page.
    pending: Vec<u8>,
    groups: Vec<Group>,
    flows: Vec<Flow>,
    styles: HashMap<i32, Style>,
    /// The style being defined in the style sheet.
    defining: Option<(i32, Style)>,
    /// The next group is a destination marked ignorable (`\*`).
    starred: bool,
    /// Fallback characters still to skip after a `\uN`.
    skip: usize,
    info: DocumentInfo,
    title: String,
    author: String,
    atn_author: String,
    /// Notes read so far, by kind, for their numbers.
    footnotes: u32,
    endnotes: u32,
    /// Each font's code page, by its number, from its character set; the
    /// font being defined in the font table; the document's default font.
    fonts: HashMap<i32, &'static encoding_rs::Encoding>,
    font_def: Option<i32>,
    deff: Option<i32>,
    /// The high half of a surrogate pair a `\uN` gave, waiting for its low
    /// half.
    high: Option<u32>,
    /// What the document may still make; past it, why the file is refused.
    budget: Budget,
    over: Option<String>,
}

impl<'a> Reader<'a> {
    fn group(&self) -> Group {
        *self.groups.last().unwrap_or(&Group {
            dest: Dest::Flow(0),
            uc: 1,
            font: None,
        })
    }

    /// The code page text is in where it is: its font's, else the
    /// document's.
    fn encoding(&self) -> &'static encoding_rs::Encoding {
        self.group()
            .font
            .or(self.deff)
            .and_then(|f| self.fonts.get(&f).copied())
            .unwrap_or(self.encoding)
    }

    fn flow(&mut self) -> Option<&mut Flow> {
        match self.group().dest {
            Dest::Flow(i) => self.flows.get_mut(i),
            _ => None,
        }
    }

    /// Decode the pending bytes in the code page and add them where the
    /// group's text goes.
    fn flush_bytes(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let text = self
            .encoding()
            .decode_without_bom_handling(&bytes)
            .0
            .into_owned();
        self.push_str(&text);
    }

    fn push_str(&mut self, s: &str) {
        match self.group().dest {
            Dest::Flow(_) => {
                if let Some(f) = self.flow() {
                    f.para.push_str(s);
                }
            }
            Dest::Title => self.title.push_str(s),
            Dest::Author => self.author.push_str(s),
            Dest::AtnAuthor => self.atn_author.push_str(s),
            Dest::Style => {
                if let Some((_, st)) = self.defining.as_mut() {
                    st.name.push_str(s);
                }
            }
            Dest::Skip | Dest::Fonts => {}
        }
    }

    /// A byte of text: skipped where it is a `\uN` fallback, else decoded
    /// with its neighbours.
    fn text_byte(&mut self, b: u8) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        self.pending.push(b);
    }

    fn text_char(&mut self, c: char) {
        self.flush_bytes();
        let mut buf = [0u8; 4];
        self.push_str(c.encode_utf8(&mut buf));
    }

    /// Start a new flow (a note, a comment, a header) for the group; past
    /// [`MAX_FLOWS`], the file is refused and the group's text goes nowhere.
    fn open_flow(&mut self, evidence: &'static str, label: String) {
        self.flush_bytes();
        if self.flows.len() > MAX_FLOWS {
            self.over.get_or_insert_with(|| {
                format!("more than {MAX_FLOWS} notes, comments, headers and footers")
            });
            self.set_dest(Dest::Skip);
            return;
        }
        self.flows.push(Flow {
            evidence,
            label,
            ..Flow::default()
        });
        let i = self.flows.len() - 1;
        if let Some(g) = self.groups.last_mut() {
            g.dest = Dest::Flow(i);
        }
    }

    fn set_dest(&mut self, dest: Dest) {
        self.flush_bytes();
        if let Some(g) = self.groups.last_mut() {
            g.dest = dest;
        }
    }

    fn control(&mut self, word: &str, param: Option<i32>) {
        let in_flow = matches!(self.group().dest, Dest::Flow(_));
        // `\*` marks the destination this word opens, and only it.
        let starred = std::mem::take(&mut self.starred);
        // Destinations first: they decide where the group's text goes.
        match word {
            "fonttbl" => {
                self.set_dest(Dest::Fonts);
                return;
            }
            "colortbl" | "listtable" | "listoverridetable" | "revtbl" | "rsidtbl" | "generator"
            | "xmlnstbl" | "themedata" | "colorschememapping" | "datastore" | "latentstyles"
            | "filetbl" | "pgdsctbl" | "docvar" | "object" | "objdata" | "shpinst" | "sp"
            | "fldinst" | "listtext" | "pntext" | "pntxta" | "pntxtb" | "bkmkstart" | "bkmkend"
            | "atnid" | "atndate" | "atnref" | "mmathPr" | "userprops" | "operator" | "comment"
            | "doccomm" | "subject" | "keywords" | "company" | "category" | "manager"
            | "hlinkbase" | "template" | "nonshppict" | "wgrffmtfilter" | "printim" | "revtim"
            | "buptim" | "passwordhash" | "xform" => {
                self.set_dest(Dest::Skip);
                return;
            }
            "stylesheet" => {
                self.set_dest(Dest::Skip);
                // Its entries are groups below, read as styles.
                if let Some(g) = self.groups.last_mut() {
                    g.dest = Dest::Style;
                }
                return;
            }
            "info" => {
                self.set_dest(Dest::Skip);
                return;
            }
            "title" if self.groups.len() >= 2 => {
                self.set_dest(Dest::Title);
                return;
            }
            "author" if self.groups.len() >= 2 => {
                self.set_dest(Dest::Author);
                return;
            }
            "creatim" => {
                self.set_dest(Dest::Skip);
                return;
            }
            "atnauthor" => {
                self.atn_author.clear();
                self.set_dest(Dest::AtnAuthor);
                return;
            }
            "pict" => {
                self.flush_bytes();
                if let Some(f) = self.flow() {
                    f.out.push(Element::new("doco:Figure", "", "rtf", "rtf"));
                }
                self.set_dest(Dest::Skip);
                return;
            }
            "header" | "headerl" | "headerr" | "headerf" => {
                self.open_flow("header", String::new());
                return;
            }
            "footer" | "footerl" | "footerr" | "footerf" => {
                self.open_flow("footer", String::new());
                return;
            }
            "footnote" => {
                self.footnotes += 1;
                let n = self.footnotes.to_string();
                self.open_flow("footnote", n);
                return;
            }
            "ftnalt" => {
                // This footnote is an endnote: renumber it as one.
                if let Some(f) = self.flow().filter(|f| f.evidence == "footnote") {
                    f.evidence = "endnote";
                    self.footnotes -= 1;
                    self.endnotes += 1;
                    let n = roman(self.endnotes);
                    if let Some(f) = self.flow() {
                        f.label = n;
                    }
                }
                return;
            }
            "annotation" => {
                let author = self.atn_author.trim().to_string();
                let label = if author.is_empty() {
                    String::new()
                } else {
                    format!("{author}:")
                };
                self.open_flow("comment", label);
                return;
            }
            _ => {}
        }
        // A picture's container, whose picture is read as one.
        if word == "shppict" {
            return;
        }
        // An ignorable destination nothing here reads.
        if starred {
            self.set_dest(Dest::Skip);
            return;
        }
        match word {
            "ansicpg" => {
                if let Some(enc) = param.and_then(code_page) {
                    self.encoding = enc;
                }
            }
            "deff" => self.deff = param,
            "f" if self.group().dest == Dest::Fonts => self.font_def = param,
            "fcharset" if self.group().dest == Dest::Fonts => {
                if let (Some(f), Some(enc)) = (self.font_def, param.and_then(charset)) {
                    self.fonts.insert(f, enc);
                }
            }
            "f" => {
                if let Some(g) = self.groups.last_mut() {
                    g.font = param;
                }
            }
            "uc" => {
                if let Some(g) = self.groups.last_mut() {
                    g.uc = param.unwrap_or(1).clamp(0, 16) as usize;
                }
            }
            "u" => {
                let n = param.unwrap_or(0);
                let n = if n < 0 { n + 65536 } else { n } as u32;
                // A character past the first plane is two `\uN`, its
                // surrogate halves.
                let c = match (self.high.take(), n) {
                    (_, 0xD800..=0xDBFF) => {
                        self.high = Some(n);
                        None
                    }
                    (Some(h), 0xDC00..=0xDFFF) => {
                        char::from_u32(0x10000 + ((h - 0xD800) << 10) + (n - 0xDC00))
                    }
                    (_, n) => char::from_u32(n),
                };
                if let Some(c) = c {
                    self.text_char(c);
                }
                self.skip = self.group().uc;
            }
            "s" if self.group().dest == Dest::Style => {
                self.defining = Some((param.unwrap_or(0), Style::default()));
            }
            "outlinelevel" if self.group().dest == Dest::Style => {
                if let Some((_, st)) = self.defining.as_mut() {
                    st.level = param.map(|l| (l.clamp(0, 8) as usize) + 1);
                }
            }
            _ if !in_flow => {}
            "par" => self.end_paragraph(),
            "line" => self.text_char('\n'),
            "tab" => self.text_char(' '),
            "emdash" => self.text_char('—'),
            "endash" => self.text_char('–'),
            "bullet" => self.text_char('•'),
            "lquote" => self.text_char('‘'),
            "rquote" => self.text_char('’'),
            "ldblquote" => self.text_char('“'),
            "rdblquote" => self.text_char('”'),
            "chftn" => {
                self.flush_bytes();
                // Inside a note, its number; in the body, the reference
                // mark, which adds nothing to the body's text.
                let label = self
                    .flow()
                    .filter(|f| matches!(f.evidence, "footnote" | "endnote"))
                    .map(|f| std::mem::take(&mut f.label));
                if let Some(l) = label {
                    self.push_str(&l);
                }
            }
            "pard" => {
                self.flush_bytes();
                if let Some(f) = self.flow() {
                    f.in_table = false;
                    f.level = None;
                    f.style = None;
                    f.list = false;
                }
            }
            "trowd" => {
                if let Some(f) = self.flow() {
                    f.defs.clear();
                    f.def = CellDef::default();
                }
            }
            "clmrg" => {
                if let Some(f) = self.flow() {
                    f.def.merged_left = true;
                }
            }
            "clvmrg" => {
                if let Some(f) = self.flow() {
                    f.def.merged_down = true;
                }
            }
            "cellx" => {
                if let Some(f) = self.flow() {
                    // No more cells are kept than the tables may hold.
                    if f.defs.len() < MAX_TABLE_CELLS {
                        let def = std::mem::take(&mut f.def);
                        f.defs.push(CellDef {
                            right: param.unwrap_or(0),
                            ..def
                        });
                    }
                }
            }
            "intbl" => {
                if let Some(f) = self.flow() {
                    f.in_table = true;
                }
            }
            "outlinelevel" => {
                if let Some(f) = self.flow() {
                    f.level = param.map(|l| (l.clamp(0, 8) as usize) + 1);
                }
            }
            "s" => {
                if let Some(f) = self.flow() {
                    f.style = param;
                }
            }
            "ls" | "ilvl" => {
                if let Some(f) = self.flow() {
                    f.list = true;
                }
            }
            "cell" | "nestcell" => {
                self.flush_bytes();
                if let Some(f) = self.flow() {
                    let text = tidy(&std::mem::take(&mut f.para));
                    f.cell.push(text);
                    let cell = std::mem::take(&mut f.cell)
                        .into_iter()
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    f.row.push(cell);
                }
            }
            "row" | "nestrow" => {
                self.flush_bytes();
                if let Dest::Flow(i) = self.group().dest {
                    if let Some(f) = self.flows.get_mut(i) {
                        end_row(f, &mut self.budget, &mut self.over);
                    }
                }
            }
            _ => {}
        }
    }

    /// End the paragraph being read: into its table's cell, or out as an
    /// element.
    fn end_paragraph(&mut self) {
        self.flush_bytes();
        let styles = &self.styles;
        let Dest::Flow(i) = self.group().dest else {
            return;
        };
        let Some(f) = self.flows.get_mut(i) else {
            return;
        };
        let text = tidy(&std::mem::take(&mut f.para));
        if f.in_table {
            if !text.is_empty() {
                f.cell.push(text);
            }
            return;
        }
        flush_table(f, &mut self.budget, &mut self.over);
        if text.is_empty() {
            return;
        }
        let style = f.style.and_then(|s| styles.get(&s));
        let level = f.level.or_else(|| style.and_then(|s| s.level)).or_else(|| {
            style.and_then(|s| {
                s.name
                    .trim()
                    .to_lowercase()
                    .strip_prefix("heading ")
                    .and_then(|n| n.trim().parse::<usize>().ok())
            })
        });
        let title = style.is_some_and(|s| s.name.trim().eq_ignore_ascii_case("title"));
        // A numbered heading (`1. Introduction`) is in a list too: Word and
        // LibreOffice number headings with one. Its level says what it is.
        let (kind, level) = match (level, title) {
            (_, true) => ("doco:Title", None),
            (Some(l), _) if (1..=9).contains(&l) => ("doco:SectionTitle", Some(l.min(6))),
            _ if f.list => ("doc:ListItem", None),
            _ => ("doco:Paragraph", None),
        };
        if let Err(e) = spend_element(&mut self.budget, &text) {
            self.over.get_or_insert(e.to_string());
            return;
        }
        f.out.push(Element {
            level,
            ..Element::new(kind, text, "rtf", "rtf")
        });
    }

    /// Close a group: record a style defined in it, finish a flow it opened.
    fn close(&mut self) {
        self.flush_bytes();
        let Some(g) = self.groups.pop() else {
            return;
        };
        let parent = self.group();
        match g.dest {
            Dest::Style if parent.dest == Dest::Style => {
                if let Some((n, mut st)) = self.defining.take() {
                    st.name = st.name.trim_end_matches(';').trim().to_string();
                    self.styles.insert(n, st);
                }
            }
            Dest::Flow(i) if parent.dest != Dest::Flow(i) => {
                // A note, comment, header or footer ends with its group:
                // its last paragraph, even without a `\par`.
                self.groups.push(g);
                self.end_paragraph();
                if let Some(f) = self.flows.get_mut(i) {
                    flush_table(f, &mut self.budget, &mut self.over);
                }
                self.groups.pop();
            }
            _ => {}
        }
    }

    fn run(&mut self) -> Result<(), RtfError> {
        let b = self.bytes;
        while self.at < b.len() {
            let c = b[self.at];
            self.at += 1;
            match c {
                b'{' => {
                    if self.groups.len() >= MAX_DEPTH {
                        return Err(RtfError::TooDeep);
                    }
                    self.flush_bytes();
                    let g = self.group();
                    self.groups.push(g);
                    self.skip = 0;
                }
                b'}' => {
                    self.close();
                    self.starred = false;
                    self.skip = 0;
                }
                b'\\' => self.escape(),
                b'\r' | b'\n' => {}
                _ => self.text_byte(c),
            }
        }
        self.flush_bytes();
        Ok(())
    }

    /// A control word or symbol after its backslash.
    fn escape(&mut self) {
        let b = self.bytes;
        let Some(&c) = b.get(self.at) else {
            return;
        };
        if c.is_ascii_alphabetic() {
            let start = self.at;
            while self.at < b.len() && b[self.at].is_ascii_alphabetic() && self.at - start < 32 {
                self.at += 1;
            }
            let word = String::from_utf8_lossy(&b[start..self.at]).into_owned();
            let neg = b.get(self.at) == Some(&b'-');
            if neg {
                self.at += 1;
            }
            let digits = self.at;
            while self.at < b.len() && b[self.at].is_ascii_digit() && self.at - digits < 10 {
                self.at += 1;
            }
            let param = std::str::from_utf8(&b[digits..self.at])
                .ok()
                .and_then(|d| d.parse::<i64>().ok())
                .map(|n| if neg { -n } else { n }.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
            if b.get(self.at) == Some(&b' ') {
                self.at += 1;
            }
            if word == "bin" {
                // Binary data: skipped whole, never past the end.
                let n = param.unwrap_or(0).max(0) as usize;
                self.at = self.at.saturating_add(n).min(b.len());
                return;
            }
            // A control word counts as one fallback character of a `\uN`.
            if self.skip > 0 && word != "u" {
                self.skip -= 1;
                return;
            }
            self.flush_bytes();
            self.control(&word, param);
            return;
        }
        self.at += 1;
        match c {
            b'\'' => {
                let hex = b.get(self.at..self.at + 2).unwrap_or(b"");
                self.at = (self.at + 2).min(b.len());
                if let Some(v) = std::str::from_utf8(hex)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    self.text_byte(v);
                }
            }
            b'*' => self.starred = true,
            b'~' => self.text_char('\u{a0}'),
            b'_' => self.text_char('-'),
            b'-' => {}
            b'\r' | b'\n' => {
                if matches!(self.group().dest, Dest::Flow(_)) {
                    self.end_paragraph();
                }
            }
            b'\\' | b'{' | b'}' => self.text_byte(c),
            _ => {}
        }
    }
}

/// An element about to be made, with its text, spent from `budget`.
fn spend_element(budget: &mut Budget, text: &str) -> Result<(), TooLarge> {
    budget.spend_elements(1)?;
    budget.spend_text(text)
}

/// End the row being read, if one is: its cells and the copy of its
/// definitions it keeps are spent from `budget` before the copy is made, a
/// row definition of millions of cells being a few bytes a cell, and the
/// rows after it a few bytes each. Past the budget, the file is refused
/// (`over`) and the row dropped.
fn end_row(f: &mut Flow, budget: &mut Budget, over: &mut Option<String>) {
    if f.row.is_empty() {
        return;
    }
    let row = std::mem::take(&mut f.row);
    if over.is_some() {
        return;
    }
    if let Err(e) = budget.spend_cells((row.len() + f.defs.len()) as u64) {
        over.get_or_insert(e.to_string());
        return;
    }
    f.rows.push((row, f.defs.clone()));
}

/// Close the table being read, if one is, into a `doco:Table`, its cells
/// (its rows padded to its widest) spent from `budget` before they are made;
/// past it, the file is refused (`over`) and the table is not made.
fn flush_table(f: &mut Flow, budget: &mut Budget, over: &mut Option<String>) {
    end_row(f, budget, over);
    if f.rows.is_empty() {
        return;
    }
    let rows = std::mem::take(&mut f.rows);
    if over.is_some() {
        return;
    }
    let grid = Grid::of(&rows);
    let width = grid.width;
    let size = rows.len().saturating_mul(width);
    let spent = spend_element(budget, "")
        .and_then(|()| budget.spend_cells(size as u64))
        .and_then(|()| {
            budget.spend_cell_texts(rows.iter().flat_map(|(r, _)| r).map(String::as_str))
        });
    if let Err(e) = spent {
        over.get_or_insert(e.to_string());
        return;
    }
    let mut cells = vec![String::new(); size];
    let mut left = vec![false; size];
    let mut down = vec![false; size];
    for (r, (row, defs)) in rows.into_iter().enumerate() {
        let at = r * width;
        match grid.spans(&defs, row.len()) {
            Some(spans) => {
                for ((text, (c0, c1)), def) in row.into_iter().zip(spans).zip(&defs) {
                    // A cell merged into the one before it or above it
                    // shows nothing of its own.
                    if !(def.merged_left || def.merged_down) {
                        cells[at + c0] = text;
                    }
                    left[at + c0] = def.merged_left;
                    down[at + c0] = def.merged_down;
                    for c in c0 + 1..c1 {
                        left[at + c] = true;
                    }
                }
            }
            None => {
                for (c, text) in row.into_iter().enumerate() {
                    cells[at + c] = text;
                }
            }
        }
    }
    let rows: Vec<Vec<String>> = cells.chunks(width).map(<[String]>::to_vec).collect();
    let text = rows
        .iter()
        .map(|r| r.join(" | "))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = Element::new("doco:Table", text, "rtf", "rtf");
    e.cells = Some(rows);
    e.merged_left = left.iter().any(|x| *x).then_some(left);
    e.merged_down = down.iter().any(|x| *x).then_some(down);
    // What DoCO copies of it: a merged value in each cell it covers.
    if let Err(err) = budget.spend_copies(&e) {
        over.get_or_insert(err.to_string());
        return;
    }
    f.out.push(e);
}

/// A table's columns: the right edges its rows' cells state, all of them,
/// in order; as many as its widest row has cells where they are fewer.
struct Grid {
    edges: Vec<i32>,
    width: usize,
}

impl Grid {
    fn of(rows: &[(Vec<String>, Vec<CellDef>)]) -> Grid {
        let widest = rows.iter().map(|(r, _)| r.len()).max().unwrap_or(0);
        let mut edges: Vec<i32> = rows
            .iter()
            .filter(|(r, d)| d.len() == r.len())
            .flat_map(|(_, d)| d.iter().map(|c| c.right))
            .collect();
        edges.sort_unstable();
        edges.dedup();
        let width = edges.len().max(widest);
        Grid { edges, width }
    }

    /// The columns each of a row's `cells` covers, `[c0, c1)`, by its
    /// definitions; `None` where they do not state each cell's edge, state
    /// them out of order, or the table has fewer edges than columns.
    fn spans(&self, defs: &[CellDef], cells: usize) -> Option<Vec<(usize, usize)>> {
        if defs.len() != cells || self.edges.len() < self.width {
            return None;
        }
        let mut out = Vec::with_capacity(cells);
        let mut c0 = 0usize;
        for d in defs {
            let c1 = self.edges.binary_search(&d.right).ok()? + 1;
            if c1 <= c0 {
                return None;
            }
            out.push((c0, c1));
            c0 = c1;
        }
        Some(out)
    }
}

/// A paragraph's text tidied: each line's runs of whitespace one space.
fn tidy(s: &str) -> String {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The encoding a font's character set (`\fcharsetN`) puts its text in;
/// `None` for the ANSI and default sets, which are the document's code
/// page, and for those with no such encoding (Symbol, OEM).
fn charset(n: i32) -> Option<&'static encoding_rs::Encoding> {
    code_page(match n {
        77 => 10000,
        128 => 932,
        129 => 949,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        _ => return None,
    })
}

/// The encoding a Windows code page names.
fn code_page(n: i32) -> Option<&'static encoding_rs::Encoding> {
    let label = match n {
        65001 => "utf-8".to_string(),
        10000 => "macintosh".to_string(),
        932 => "shift_jis".to_string(),
        936 => "gbk".to_string(),
        949 => "euc-kr".to_string(),
        950 => "big5".to_string(),
        874 => "windows-874".to_string(),
        n => format!("windows-{n}"),
    };
    encoding_rs::Encoding::for_label(label.as_bytes())
}

fn roman(mut n: u32) -> String {
    let mut out = String::new();
    for (v, s) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// Parse an RTF document into elements: the header, the body, its notes,
/// the footer, then its comments; with the title and author its `\info`
/// declares.
pub fn parse_with_info(bytes: &[u8]) -> Result<(Vec<Element>, DocumentInfo), RtfError> {
    parse_within(bytes, Budget::default())
}

/// [`parse_with_info`], what the document makes spent from `budget`.
fn parse_within(bytes: &[u8], budget: Budget) -> Result<(Vec<Element>, DocumentInfo), RtfError> {
    if !is_rtf(bytes) {
        return Err(RtfError::NotRtf);
    }
    let mut r = Reader {
        bytes,
        at: 0,
        encoding: encoding_rs::WINDOWS_1252,
        pending: Vec::new(),
        groups: Vec::new(),
        flows: vec![Flow {
            evidence: "rtf",
            ..Flow::default()
        }],
        styles: HashMap::new(),
        defining: None,
        starred: false,
        skip: 0,
        info: DocumentInfo::default(),
        title: String::new(),
        author: String::new(),
        atn_author: String::new(),
        footnotes: 0,
        endnotes: 0,
        fonts: HashMap::new(),
        font_def: None,
        deff: None,
        high: None,
        budget,
        over: None,
    };
    r.run()?;
    // What is left of the body: a last paragraph with no `\par`, a table.
    r.groups.clear();
    r.end_paragraph();
    flush_table(&mut r.flows[0], &mut r.budget, &mut r.over);
    if let Some(why) = r.over.take() {
        return Err(RtfError::TooLarge(why));
    }
    let mut by = |evidence: &str| -> Vec<Element> {
        let mut out: Vec<Element> = Vec::new();
        // What the headers (or footers) read before this one said: one read
        // for each kind of page says the same thing more than once, but a
        // line a header repeats is its own.
        let mut said: HashSet<String> = HashSet::new();
        for f in r.flows.iter_mut().filter(|f| f.evidence == evidence) {
            let mut this: Vec<String> = Vec::new();
            let mut first = true;
            for mut e in std::mem::take(&mut f.out) {
                if e.kind != "doco:Table" && e.kind != "doco:Figure" && evidence != "rtf" {
                    e.kind = "doco:Paragraph".into();
                    e.level = None;
                    if first && !f.label.is_empty() {
                        e.text = format!("{} {}", f.label, e.text);
                    }
                }
                first = false;
                e.evidence = match evidence {
                    "rtf" => "rtf",
                    "header" => "header",
                    "footer" => "footer",
                    "footnote" => "footnote",
                    "endnote" => "endnote",
                    _ => "comment",
                };
                if matches!(evidence, "header" | "footer") {
                    if said.contains(&e.text) {
                        continue;
                    }
                    this.push(e.text.clone());
                }
                out.push(e);
            }
            said.extend(this);
        }
        out
    };
    let mut out = by("header");
    out.extend(by("rtf"));
    out.extend(by("footnote"));
    out.extend(by("endnote"));
    out.extend(by("footer"));
    out.extend(by("comment"));
    for (i, e) in out.iter_mut().enumerate() {
        e.id = format!("elem-{:05}", i + 1);
    }
    let mut info = std::mem::take(&mut r.info);
    let title = tidy(&r.title);
    info.title = (!title.is_empty()).then_some(title);
    let author = tidy(&r.author);
    info.creators = (!author.is_empty()).then_some(author).into_iter().collect();
    Ok((out, info))
}

/// [`parse_with_info`], the elements alone.
pub fn parse(bytes: &[u8]) -> Result<Vec<Element>, RtfError> {
    parse_with_info(bytes).map(|(e, _)| e)
}

#[cfg(test)]
mod tests;
