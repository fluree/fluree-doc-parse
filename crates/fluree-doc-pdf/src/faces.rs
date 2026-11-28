//! What a PDF's font dictionaries say about the weight of each face it draws.
//!
//! The interpreter reads a weight from the font program only where the
//! program is TrueType or OpenType, and there it reads the program's own
//! weight class, which a subsetter can get wrong: a data sheet's subset of
//! `NimbusSansL-Regu` states 700, and its whole body read as bold. From a
//! Type 1 or CFF program (`/FontFile3`) it reads none. The PDF says more:
//! the font's name (`HelveticaNeueLTStd-Bd`, `Sohne-Halbfett`) and its
//! descriptor's `/FontWeight` and `/StemV`. They are read here from the
//! page's resources, keyed as the interpreter keys the fonts it draws with,
//! so a glyph finds what its font's dictionary says.
//!
//! The names a Type 1 or CFF program gives its glyphs are read here too, for
//! the glyphs the interpreter reads no text for ([`Faces::named`]).

use std::collections::{HashMap, HashSet};

use hayro_interpret::CacheKey;
use hayro_syntax::object::dict::keys::{
    BASE_ENCODING, BASE_FONT, CID_FONT_TYPE2, DESCENDANT_FONTS, DIFFERENCES, ENCODING, FIRST_CHAR,
    FLAGS, FONT, FONT_DESC, FONT_FILE, FONT_FILE2, FONT_FILE3, FONT_WEIGHT, FORM, ITALIC_ANGLE,
    MAC_ROMAN_ENCODING, MM_TYPE1, RESOURCES, STANDARD_ENCODING, STEM_V, SUBTYPE, TO_UNICODE,
    TRUE_TYPE, TYPE0, TYPE1, WIDTHS, WIN_ANSI_ENCODING, XOBJECT,
};
use hayro_syntax::object::{Array, Dict, Name, Object, Stream};
use hayro_syntax::page::Page;

/// How deep forms inside forms are followed for their fonts.
const MAX_FORM_DEPTH: u32 = 16;

/// What one font's dictionary says about its weight.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Face {
    /// The weight a word in its name states (`Bold`, `Lt`, `Halbfett`).
    pub named: Option<u32>,
    /// The descriptor's `/FontWeight`.
    pub declared: Option<u32>,
    /// The descriptor's `/StemV`, the width of its vertical stems.
    pub stem: Option<f32>,
    /// An italic or oblique face, as its name says (`MinionPro-BoldIt`,
    /// `Univers-BoldOblique`) or its descriptor does (an `/ItalicAngle`, the
    /// italic flag).
    pub italic: bool,
}

/// The faces of the fonts a document draws with, by the interpreter's key
/// for each font, gathered page by page.
#[derive(Default)]
pub(crate) struct Faces<'a> {
    faces: HashMap<u128, Face>,
    /// Letters each font's ToUnicode map gives in the wrong case, by the
    /// interpreter's key for the font ([`recased`]).
    recase: HashMap<u128, Vec<Recase>>,
    /// Resource dictionaries already read, so a form every page draws is
    /// read once.
    read: HashSet<u128>,
    /// The embedded program of each simple Type 1 font, by the interpreter's
    /// key for the font, until a glyph drawn in it needs its names.
    programs: HashMap<u128, Program<'a>>,
    /// The text each program's glyph names give, by glyph ID, once read
    /// ([`program_texts`]).
    names: HashMap<u128, Vec<Option<String>>>,
    /// The embedded TrueType program of each CID font set in one, by the
    /// interpreter's key for the font, until a glyph drawn in it is read by
    /// its program ([`Faces::mapped`]).
    truetype: HashMap<u128, Stream<'a>>,
    /// The character each such program's own Unicode cmap gives a glyph,
    /// by glyph ID, once read ([`cmap_chars`]).
    mapped: HashMap<u128, HashMap<u32, char>>,
    /// The Type 1 program of each font whose name says it is one of TeX's
    /// Computer Modern families, with that family's layout, until a glyph
    /// drawn in it is read ([`Faces::tex`]).
    tex_programs: HashMap<u128, (crate::tex::Layout, Stream<'a>)>,
    /// What each such program's glyphs are, by glyph ID: the text its name
    /// labels it with, and the text TeX's layout gives its code. Empty for
    /// a program read by its names ([`tex_glyphs`]).
    tex: HashMap<u128, Vec<Option<TexGlyph>>>,
}

/// A glyph of a TeX font whose vector labels glyphs by their codes.
#[derive(Debug, Clone)]
struct TexGlyph {
    /// The text its name gives, which the ToUnicode map, if any, repeats.
    label: Option<String>,
    /// The text TeX's layout gives its code.
    text: &'static str,
}

/// A simple Type 1 font's embedded program: CFF (`/FontFile3`) or Type 1
/// (`/FontFile`).
#[derive(Clone)]
enum Program<'a> {
    Cff(Stream<'a>),
    Type1(Stream<'a>),
}

impl<'a> Faces<'a> {
    /// Read the fonts a page and the forms it draws can use.
    pub(crate) fn read(&mut self, page: &Page<'a>) {
        let mut resources = Some(page.resources());
        while let Some(r) = resources {
            self.fonts(&r.fonts);
            self.forms(&r.x_objects, 0);
            resources = r.parent();
        }
    }

    /// What the font the interpreter keys so says, if its dictionary was read.
    pub(crate) fn get(&self, key: u128) -> Option<&Face> {
        self.faces.get(&key)
    }

    /// The letter a glyph drawn in this font draws, where its ToUnicode map
    /// gives `read` and the font's encoding gives that letter in the other
    /// case, for a glyph of this width ([`recased`]).
    pub(crate) fn recase(&self, key: u128, read: char, width: Option<f32>) -> Option<char> {
        let width = width?;
        self.recase
            .get(&key)?
            .iter()
            .find(|r| r.read == read && (r.width - width).abs() < 0.5)
            .map(|r| r.drawn)
    }

    /// The text the font program's name for a glyph gives, for a glyph the
    /// interpreter reads none for: `h.sc`, `one.osf`, `f_i`.
    ///
    /// The interpreter reads a glyph's text from the font's ToUnicode map,
    /// else from the name its encoding or program gives the glyph, but only a
    /// name the Adobe Glyph List lists whole (`h`, `one`, `fi`). The list's
    /// own rules for reading a name go further: what follows the first
    /// period is a variant of the glyph before it (`h.sc` is a small capital
    /// h, `one.osf` an old-style 1), underscores join the glyphs of a
    /// ligature (`f_i`), and `uniXXXX` may name several characters. A book
    /// set with small capitals and old-style figures and no ToUnicode map
    /// read without them: "his collection of six" lost all but its drop
    /// cap, and a year read as nothing. Only simple Type 1 fonts with an
    /// embedded program are read, where a glyph's ID is its place in the
    /// program, which is where the interpreter takes its name from. A CID
    /// font's glyphs are numbered, not named.
    pub(crate) fn named(&mut self, key: u128, glyph: u32) -> Option<String> {
        if let Some(program) = self.programs.remove(&key) {
            self.names.insert(key, program_texts(&program));
        }
        self.names.get(&key)?.get(glyph as usize)?.clone()
    }

    /// The character a CID font's embedded TrueType program maps to a glyph
    /// in its own Unicode cmap, for a glyph the interpreter reads no text
    /// for: a ToUnicode map that leaves out codes the page draws.
    ///
    /// A producer that subsets fonts again (`KYNVLZ+YIWYLX+Arial`) can write
    /// a ToUnicode map short of the glyphs it keeps: an invoice's totals box
    /// read `a ance Due` for `Balance Due`, its `(`, `%` and `)` lost, while
    /// the program's cmap still maps each to its glyph. Where several
    /// characters map to one glyph (a space and a no-break space), the
    /// lowest is read; private-use and control characters are not.
    pub(crate) fn mapped(&mut self, key: u128, glyph: u32) -> Option<char> {
        if let Some(program) = self.truetype.remove(&key) {
            self.mapped.insert(key, cmap_chars(&program));
        }
        self.mapped.get(&key)?.get(&glyph).copied()
    }

    /// The text a glyph drawn in one of TeX's fonts is, by TeX's layout for
    /// the font's family, where the font's vector labels glyphs by their
    /// codes and `read`, what the interpreter read for it, says no more than
    /// the label: the label itself, U+FFFD or nothing. See [`crate::tex`].
    /// A ToUnicode map that reads the glyph otherwise stands.
    pub(crate) fn tex(&mut self, key: u128, glyph: u32, read: &str) -> Option<&'static str> {
        if let Some((layout, program)) = self.tex_programs.remove(&key) {
            self.tex.insert(key, tex_glyphs(layout, &program));
        }
        let g = self.tex.get(&key)?.get(glyph as usize)?.as_ref()?;
        let unread = read.is_empty() || read == "\u{FFFD}";
        (unread || g.label.as_deref() == Some(read)).then_some(g.text)
    }

    fn fonts(&mut self, fonts: &Dict<'a>) {
        if !self.read.insert(fonts.cache_key()) {
            return;
        }
        for name in fonts.keys() {
            if let Some(font) = fonts.get::<Dict<'_>>(name.as_ref()) {
                self.faces
                    .entry(font.cache_key())
                    .or_insert_with(|| face(&font));
                if let std::collections::hash_map::Entry::Vacant(e) =
                    self.recase.entry(font.cache_key())
                {
                    e.insert(recased(&font));
                }
                let key = font.cache_key();
                if !self.names.contains_key(&key) && !self.programs.contains_key(&key) {
                    if let Some(program) = program(&font) {
                        self.programs.insert(key, program);
                    }
                }
                if !self.mapped.contains_key(&key) && !self.truetype.contains_key(&key) {
                    if let Some(program) = cid_truetype(&font) {
                        self.truetype.insert(key, program);
                    }
                }
                if !self.tex.contains_key(&key) && !self.tex_programs.contains_key(&key) {
                    if let Some(tex) = tex_program(&font) {
                        self.tex_programs.insert(key, tex);
                    }
                }
            }
        }
    }

    fn forms(&mut self, x_objects: &Dict<'a>, depth: u32) {
        if depth >= MAX_FORM_DEPTH || !self.read.insert(x_objects.cache_key()) {
            return;
        }
        for name in x_objects.keys() {
            let Some(stream) = x_objects.get::<Stream<'_>>(name.as_ref()) else {
                continue;
            };
            let dict = stream.dict();
            if dict.get::<Name<'_>>(SUBTYPE).as_deref() != Some(FORM) {
                continue;
            }
            if let Some(own) = dict.get::<Dict<'_>>(RESOURCES) {
                if let Some(fonts) = own.get::<Dict<'_>>(FONT) {
                    self.fonts(&fonts);
                }
                if let Some(inner) = own.get::<Dict<'_>>(XOBJECT) {
                    self.forms(&inner, depth + 1);
                }
            }
        }
    }
}

/// A simple Type 1 font's embedded program, if it has one.
fn program<'a>(font: &Dict<'a>) -> Option<Program<'a>> {
    if !matches!(
        font.get::<Name<'_>>(SUBTYPE).as_deref(),
        Some(TYPE1 | MM_TYPE1)
    ) {
        return None;
    }
    let descriptor = font.get::<Dict<'a>>(FONT_DESC)?;
    descriptor
        .get::<Stream<'a>>(FONT_FILE3)
        .map(Program::Cff)
        .or_else(|| descriptor.get::<Stream<'a>>(FONT_FILE).map(Program::Type1))
}

/// A CID font's embedded TrueType program (`/FontFile2` under a
/// `CIDFontType2` descendant), if it has one.
fn cid_truetype<'a>(font: &Dict<'a>) -> Option<Stream<'a>> {
    if font.get::<Name<'_>>(SUBTYPE).as_deref() != Some(TYPE0) {
        return None;
    }
    let descendant = font
        .get::<Array<'a>>(DESCENDANT_FONTS)?
        .iter::<Dict<'a>>()
        .next()?;
    if descendant.get::<Name<'_>>(SUBTYPE).as_deref() != Some(CID_FONT_TYPE2) {
        return None;
    }
    descendant
        .get::<Dict<'a>>(FONT_DESC)?
        .get::<Stream<'a>>(FONT_FILE2)
}

/// The character a TrueType program's Unicode cmap subtables (Unicode, or
/// Windows Unicode BMP or full) map to each glyph: the lowest, where
/// several do, leaving out private-use and control characters.
fn cmap_chars(program: &Stream<'_>) -> HashMap<u32, char> {
    use read_fonts::TableProvider;
    let mut chars: HashMap<u32, char> = HashMap::new();
    let Ok(data) = program.decoded() else {
        return chars;
    };
    let Ok(font) = read_fonts::FontRef::new(&data) else {
        return chars;
    };
    let Ok(cmap) = font.cmap() else {
        return chars;
    };
    let limits = read_fonts::tables::cmap::CmapIterLimits {
        max_char: 0x10FFFF,
        glyph_count: font
            .maxp()
            .map_or(u16::MAX as u32, |m| m.num_glyphs() as u32),
    };
    for record in cmap.encoding_records() {
        let unicode = match record.platform_id() {
            read_fonts::tables::cmap::PlatformId::Unicode => true,
            read_fonts::tables::cmap::PlatformId::Windows => {
                matches!(record.encoding_id(), 1 | 10)
            }
            _ => false,
        };
        let Some(subtable) = unicode
            .then(|| record.subtable(cmap.offset_data()).ok())
            .flatten()
        else {
            continue;
        };
        for (code, gid) in subtable.iter_with_limits(limits) {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            let private = matches!(code, 0xE000..=0xF8FF | 0xF0000..=0x10FFFF);
            if c.is_control() || private || gid.to_u32() == 0 {
                continue;
            }
            chars
                .entry(gid.to_u32())
                .and_modify(|k| *k = (*k).min(c))
                .or_insert(c);
        }
    }
    chars
}

/// A simple Type 1 font's embedded Type 1 program, with its layout, where
/// the font's name says it is one of TeX's families ([`crate::tex::layout_of`]).
fn tex_program<'a>(font: &Dict<'a>) -> Option<(crate::tex::Layout, Stream<'a>)> {
    if font.get::<Name<'_>>(SUBTYPE).as_deref() != Some(TYPE1) {
        return None;
    }
    let layout = crate::tex::layout_of(font.get::<Name<'_>>(BASE_FONT)?.as_str())?;
    let program = font
        .get::<Dict<'a>>(FONT_DESC)?
        .get::<Stream<'a>>(FONT_FILE)?;
    Some((layout, program))
}

/// What each glyph of a TeX font's program is ([`TexGlyph`]), by glyph ID,
/// where its own vector labels glyphs by their codes; else nothing. A
/// predefined vector (StandardEncoding) is the names a program reached by
/// name gives, not a TeX layout, and is never read so.
fn tex_glyphs(layout: crate::tex::Layout, program: &Stream<'_>) -> Vec<Option<TexGlyph>> {
    let Ok(data) = program.decoded() else {
        return Vec::new();
    };
    let Ok(font) = read_fonts::ps::type1::Type1Font::new(&data) else {
        return Vec::new();
    };
    let Some(vector) = font.encoding().filter(|e| e.predefined().is_none()) else {
        return Vec::new();
    };
    if !crate::tex::labelled_by_codes(|code| vector.glyph_name(code)) {
        return Vec::new();
    }
    let mut glyphs = vec![None; font.num_glyphs() as usize];
    for code in 0u8..128 {
        let (Some(gid), Some(text)) = (vector.map(code), crate::tex::text(layout, code)) else {
            continue;
        };
        if let Some(slot) = glyphs.get_mut(gid.to_u32() as usize) {
            if slot.is_none() {
                *slot = Some(TexGlyph {
                    label: font.glyph_name(gid).and_then(name_text),
                    text,
                });
            }
        }
    }
    glyphs
}

/// The text a glyph's name gives by the Adobe Glyph List's rules: the
/// name up to its first period, each part between underscores read as a
/// listed name, `uniXXXX` (one or more characters) or `uXXXX`. A name that
/// gives none, a subsetter's `g12` or `.notdef`, gives `None`.
fn name_text(name: &str) -> Option<String> {
    let text: String = read_fonts::ps::agl::name_to_chars(name).collect();
    (!text.is_empty()).then_some(text)
}

/// The text each glyph's name in a program gives ([`name_text`]), by glyph
/// ID; see [`Faces::named`].
fn program_texts(program: &Program<'_>) -> Vec<Option<String>> {
    match program {
        Program::Cff(stream) => {
            let Ok(data) = stream.decoded() else {
                return Vec::new();
            };
            let Ok(font) = read_fonts::ps::cff::CffFontRef::new(&data, 0, None) else {
                return Vec::new();
            };
            let Some(charset) = font.charset().filter(|_| !font.is_cid()) else {
                return Vec::new();
            };
            let mut texts = vec![None; font.num_glyphs() as usize];
            for (gid, sid) in charset.iter() {
                let name = font.string(sid).and_then(|b| std::str::from_utf8(b).ok());
                if let (Some(slot), Some(name)) = (texts.get_mut(gid.to_u32() as usize), name) {
                    *slot = name_text(name);
                }
            }
            texts
        }
        Program::Type1(stream) => {
            let Ok(data) = stream.decoded() else {
                return Vec::new();
            };
            let Ok(font) = read_fonts::ps::type1::Type1Font::new(&data) else {
                return Vec::new();
            };
            let mut texts = vec![None; font.num_glyphs() as usize];
            for (gid, name) in font.glyph_names() {
                if let Some(slot) = texts.get_mut(gid.to_u32() as usize) {
                    *slot = name_text(name);
                }
            }
            texts
        }
    }
}

/// A letter a font's ToUnicode map gives in the wrong case: glyphs of this
/// width that the map reads as `read` draw `drawn`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recase {
    read: char,
    width: f32,
    drawn: char,
}

/// The codes of a simple font whose ToUnicode map gives a letter in the
/// other case than its encoding names: `<46> <0066>`, F read as f, in a map
/// a producer folded the case of. The encoding is the authority on the glyph
/// drawn: the font draws the glyph the encoding names, `/F`, and the width
/// the PDF gives the code is that glyph's (611 for F against 278 for f).
/// Only a standard base encoding (WinAnsi, MacRoman, Standard), with its
/// `/Differences`, is read, where a code's name is known without the font
/// program; and only letters that differ in case alone. A map that reads a
/// glyph as something else on purpose, a small capital's `f.sc` as `F`, a
/// ligature, a symbol font's glyph, is no case of one letter for another.
/// Codes are told apart by width, as a glyph is drawn: a reading and width
/// that another code shares, read as the map reads it, is left alone.
fn recased(font: &Dict<'_>) -> Vec<Recase> {
    let simple = matches!(
        font.get::<Name<'_>>(SUBTYPE).as_deref(),
        Some(TYPE1 | MM_TYPE1 | TRUE_TYPE)
    );
    let Some(encoding) = font.get::<Dict<'_>>(ENCODING) else {
        return Vec::new();
    };
    let standard = matches!(
        encoding.get::<Name<'_>>(BASE_ENCODING).as_deref(),
        Some(WIN_ANSI_ENCODING | MAC_ROMAN_ENCODING | STANDARD_ENCODING)
    );
    if !simple || !standard {
        return Vec::new();
    }
    let Some(map) = font
        .get::<Stream<'_>>(TO_UNICODE)
        .and_then(|s| s.decoded().ok())
        .and_then(|d| hayro_interpret::hayro_cmap::CMap::parse(&d, |_| None))
    else {
        return Vec::new();
    };
    // Each code's name: `/Differences` over the base, whose letters stand at
    // their ASCII codes in all three standard encodings.
    let mut names: HashMap<u32, Option<char>> = HashMap::new();
    if let Some(differences) = encoding.get::<Array<'_>>(DIFFERENCES) {
        let mut code = 0_i64;
        for obj in differences.iter::<Object<'_>>() {
            match obj {
                Object::Number(n) => code = n.as_i64(),
                Object::Name(n) => {
                    if let Ok(c) = u32::try_from(code) {
                        let mut chars = n.as_str().chars();
                        let one = match (chars.next(), chars.next()) {
                            (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
                            _ => None,
                        };
                        names.insert(c, one);
                    }
                    code += 1;
                }
                _ => {}
            }
        }
    }
    let named = |code: u32| match names.get(&code) {
        Some(n) => *n,
        None => char::from_u32(code).filter(|c| c.is_ascii_alphabetic()),
    };
    let first = font.get::<usize>(FIRST_CHAR).unwrap_or(0) as u32;
    let widths: Vec<f32> = font
        .get::<Array<'_>>(WIDTHS)
        .map(|w| w.iter::<f32>().collect())
        .unwrap_or_default();
    let width = |code: u32| {
        code.checked_sub(first)
            .and_then(|i| widths.get(i as usize).copied())
    };
    let read = |code: u32| match map.lookup_bf_string(code) {
        Some(hayro_interpret::hayro_cmap::BfString::Char(c)) => Some(c),
        _ => None,
    };
    let mut out: Vec<Recase> = Vec::new();
    let mut plain: Vec<(char, f32)> = Vec::new();
    for code in 0..=255u32 {
        let (Some(read), Some(width)) = (read(code), width(code)) else {
            continue;
        };
        match named(code) {
            Some(drawn) if drawn != read && drawn.eq_ignore_ascii_case(&read) => {
                out.push(Recase { read, width, drawn })
            }
            _ => plain.push((read, width)),
        }
    }
    out.retain(|r| {
        !plain
            .iter()
            .any(|&(c, w)| c == r.read && (w - r.width).abs() < 0.5)
    });
    out
}

/// What a font dictionary says about its face. A composite font's face is
/// its descendant's, whose name carries no encoding suffix.
fn face(font: &Dict<'_>) -> Face {
    let descendant = font
        .get::<Array<'_>>(DESCENDANT_FONTS)
        .and_then(|a| a.iter::<Dict<'_>>().next());
    let font = descendant.as_ref().unwrap_or(font);
    let named = font
        .get::<Name<'_>>(BASE_FONT)
        .and_then(|n| named_weight(n.as_str()));
    let descriptor = font.get::<Dict<'_>>(FONT_DESC);
    let declared = descriptor
        .as_ref()
        .and_then(|d| d.get::<f32>(FONT_WEIGHT))
        .filter(|w| (100.0..=900.0).contains(w))
        .map(|w| w.round() as u32);
    let stem = descriptor
        .as_ref()
        .and_then(|d| d.get::<f32>(STEM_V))
        .filter(|s| *s > 0.0);
    let italic = font
        .get::<Name<'_>>(BASE_FONT)
        .is_some_and(|n| named_italic(n.as_str()))
        || descriptor.as_ref().is_some_and(|d| {
            d.get::<f32>(ITALIC_ANGLE)
                .is_some_and(|a| a.abs() >= MIN_ITALIC_ANGLE)
                || d.get::<i64>(FLAGS).is_some_and(|f| f & ITALIC_FLAG != 0)
        });
    Face {
        named,
        declared,
        stem,
        italic,
    }
}

/// The descriptor flag for an italic face (bit 7 of `/Flags`).
const ITALIC_FLAG: i64 = 1 << 6;

/// Degrees of slant from which a descriptor's `/ItalicAngle` makes a face
/// italic: an upright face states 0, and an italic one 9 to 15. A degree
/// leaves room for a rounding in the producer.
const MIN_ITALIC_ANGLE: f32 = 1.0;

/// Whether a font's name names an italic or oblique face: the whole word
/// anywhere (`TimesNewRomanPS-BoldItalicMT`, `Univers-BoldOblique`,
/// `TimesItalic`), or a style's abbreviation (`MinionPro-BoldIt`,
/// `HelveticaNeueLTStd-BdIt`, `Arial-ItalicMT`, `Frutiger-Obl`).
pub(crate) fn named_italic(name: &str) -> bool {
    let name = match name.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 => rest,
        _ => name,
    };
    let lower = name.to_ascii_lowercase();
    if ["italic", "oblique", "kursiv"]
        .iter()
        .any(|w| lower.contains(w))
    {
        return true;
    }
    let Some(i) = name.find(['-', ',']) else {
        return false;
    };
    tokens(&name[i + 1..])
        .map(str::to_ascii_lowercase)
        .any(|w| matches!(w.as_str(), "it" | "ital" | "obl" | "ob"))
}

/// The weight a font's name states, if it names one.
///
/// A name is a family and a style, `HelveticaNeueLTStd-BdIt` or
/// `Arial,Bold`; the style is what follows the first hyphen or comma. Whole
/// weight words are found anywhere in the name, since some names join them
/// to the family (`TimesNewRomanPS-BoldMT`, `ArialBold`). Abbreviations and
/// German weight names are taken from the style only, where they cannot be
/// part of a family's name: `Bd`, `Md` and `Lt` in Linotype's names, `Regu`
/// and `Medi` in URW's, `Halbfett` and `Fett` in German foundries'.
///
/// URW's `Medi` is the bold face (`NimbusRomNo9L-Medi` sets every bold word
/// in a pdfTeX paper), where the full word `Medium` is the ordinary 500.
pub(crate) fn named_weight(name: &str) -> Option<u32> {
    let name = match name.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 => rest,
        _ => name,
    };
    let lower = name.to_ascii_lowercase();
    let style = lower
        .find(['-', ','])
        .map(|i| &lower[i + 1..])
        .unwrap_or("");
    // Heaviest first, so that `ExtraBold` is not read as `Bold`, nor
    // `SemiBold` as either.
    const WORDS: &[(&str, u32)] = &[
        ("extrablack", 900),
        ("ultrablack", 900),
        ("extrabold", 800),
        ("ultrabold", 800),
        ("black", 900),
        ("heavy", 800),
        ("semibold", 600),
        ("demibold", 600),
        ("bold", 700),
        ("medium", 500),
    ];
    if let Some(&(_, w)) = WORDS.iter().find(|(word, _)| lower.contains(word)) {
        return Some(w);
    }
    // German weight names, heaviest first for the same reason.
    const GERMAN: &[(&str, u32)] = &[
        ("extrafett", 800),
        ("dreiviertelfett", 700),
        ("halbfett", 600),
        ("fett", 700),
        ("kräftig", 500),
        ("kraftig", 500),
        ("kraeftig", 500),
        ("extraleicht", 200),
        ("leicht", 300),
        ("mager", 300),
        ("buch", 400),
    ];
    if let Some(&(_, w)) = GERMAN.iter().find(|(word, _)| style.contains(word)) {
        return Some(w);
    }
    // The style's own words, split where a capital starts one: `BdIt` is
    // `Bd` and `It`. A modifier joins the word after it (`UltraLight`), and
    // alone is the weight it modifies to the extreme (`Gotham-Ultra`).
    let original = &name[name.len() - style.len()..];
    let words: Vec<String> = tokens(original).map(str::to_ascii_lowercase).collect();
    let mut i = 0;
    while i < words.len() {
        let mut word = words[i].clone();
        if matches!(word.as_str(), "ultra" | "extra" | "semi" | "demi") {
            if let Some(next) = words.get(i + 1) {
                word.push_str(next);
                i += 1;
            }
        }
        i += 1;
        let w = match word.as_str() {
            "ultra" | "extra" => 900,
            "blk" | "hv" | "xbd" | "xbold" | "extrabd" | "ultrabd" => 800,
            "demi" | "dm" | "sb" | "sbd" | "smbd" | "semibd" | "demibd" => 600,
            "bd" | "medi" => 700,
            "md" | "med" => 500,
            "regu" | "regular" | "roman" | "rmn" | "normal" | "book" | "bk" => 400,
            "lt" | "light" | "lite" | "semilight" | "demilight" | "semilt" => 300,
            "th" | "thin" | "xlt" | "extralight" | "ultralight" | "extralt" | "ultralt"
            | "extrathin" | "ultrathin" | "hairline" => 200,
            _ => continue,
        };
        return Some(w);
    }
    None
}

/// A style's words: each starts at a capital or after a non-letter.
fn tokens(style: &str) -> impl Iterator<Item = &str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in style.char_indices() {
        let start = c.is_alphabetic()
            && match prev {
                None => true,
                Some(p) => !p.is_alphabetic() || (c.is_uppercase() && p.is_lowercase()),
            };
        if start {
            starts.push(i);
        }
        prev = Some(c);
    }
    let ends: Vec<usize> = starts
        .iter()
        .map(|&s| {
            style[s..]
                .char_indices()
                .skip(1)
                .find(|&(_, c)| !c.is_alphabetic() || c.is_uppercase())
                .map_or(style.len(), |(i, _)| s + i)
        })
        .collect();
    starts.into_iter().zip(ends).map(move |(s, e)| &style[s..e])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glyph_name_read_by_the_glyph_lists_rules() {
        for (name, text) in [
            // A variant: a small capital, an old-style figure, an alternate.
            ("h.sc", "h"),
            ("one.osf", "1"),
            ("T.swash", "T"),
            // A ligature's glyphs, joined by underscores.
            ("f_i", "fi"),
            ("f_f_l", "ffl"),
            // A listed ligature, and characters by their code points.
            ("fi", "\u{FB01}"),
            ("uni00660069", "fi"),
            ("u1D400", "\u{1D400}"),
            ("uni0066.sc", "f"),
        ] {
            assert_eq!(name_text(name).as_deref(), Some(text), "{name}");
        }
        // Names that say nothing of the character: a subsetter's, the
        // missing glyph's, code points not in uppercase hex.
        for name in ["g12", ".notdef", "", "uni00e9", "glyph7"] {
            assert_eq!(name_text(name), None, "{name}");
        }
    }

    #[test]
    fn an_italic_face_by_its_name() {
        for name in [
            "ABCDEF+MinionPro-BoldIt",
            "HelveticaNeueLTStd-BdIt",
            "Univers-BoldOblique",
            "TimesNewRomanPS-BoldItalicMT",
            "Arial,BoldItalic",
            "MyriadPro-SemiboldIt",
            "Frutiger-Obl",
        ] {
            assert!(named_italic(name), "{name}");
        }
        for name in [
            "MinionPro-Bold",
            "Helvetica-Bold",
            "Arial,Bold",
            "TitilliumWeb-Bold",
            "Literata-Regular",
            "AdvP41461E",
        ] {
            assert!(!named_italic(name), "{name}");
        }
    }

    #[test]
    fn weight_words_anywhere_in_the_name() {
        assert_eq!(named_weight("ABCDEF+TimesNewRomanPS-BoldMT"), Some(700));
        assert_eq!(named_weight("ArialBold"), Some(700));
        assert_eq!(named_weight("Arial,Bold"), Some(700));
        assert_eq!(named_weight("MyriadPro-Semibold"), Some(600));
        assert_eq!(named_weight("Montserrat-ExtraBold"), Some(800));
        assert_eq!(named_weight("FrutigerLTStd-UltraBlack"), Some(900));
        assert_eq!(named_weight("Roboto-Medium"), Some(500));
    }

    #[test]
    fn abbreviated_styles() {
        assert_eq!(named_weight("YKWGVH+HelveticaNeueLTStd-Bd"), Some(700));
        assert_eq!(named_weight("HelveticaNeueLTStd-BdIt"), Some(700));
        assert_eq!(named_weight("HelveticaNeueLTStd-BdCn"), Some(700));
        assert_eq!(named_weight("HelveticaNeueLTStd-Md"), Some(500));
        assert_eq!(named_weight("HelveticaNeueLTStd-Lt"), Some(300));
        assert_eq!(named_weight("HelveticaNeueLTStd-Roman"), Some(400));
        assert_eq!(named_weight("HelveticaNeueLTStd-Cn"), None);
        assert_eq!(named_weight("FranklinGothic-Demi"), Some(600));
        assert_eq!(named_weight("Univers-BlkIt"), Some(800));
        assert_eq!(named_weight("Gotham-Ultra"), Some(900));
        assert_eq!(named_weight("Gotham-UltraLight"), Some(200));
        assert_eq!(named_weight("SegoeUI-SemiLight"), Some(300));
        assert_eq!(named_weight("OpenSans-ExtraLight"), Some(200));
    }

    #[test]
    fn urw_styles() {
        assert_eq!(named_weight("NimbusSansL-Regu"), Some(400));
        assert_eq!(named_weight("NimbusSansL-Bold"), Some(700));
        assert_eq!(named_weight("NimbusRomNo9L-Medi"), Some(700));
        assert_eq!(named_weight("NimbusRomNo9L-ReguItal"), Some(400));
    }

    #[test]
    fn german_styles() {
        assert_eq!(named_weight("Sohne-Halbfett"), Some(600));
        assert_eq!(named_weight("Sohne-Fett"), Some(700));
        assert_eq!(named_weight("Sohne-Kraftig"), Some(500));
        assert_eq!(named_weight("Sohne-Buch"), Some(400));
    }

    #[test]
    fn no_weight_in_a_family_name_or_none_at_all() {
        // Abbreviations are only styles: a family that starts with one is
        // not read as it.
        assert_eq!(named_weight("Mdx-Italic"), None);
        assert_eq!(named_weight("Ltd"), None);
        assert_eq!(named_weight("CIDFont+F4"), None);
        assert_eq!(named_weight("Helvetica"), None);
        assert_eq!(named_weight("Symbol"), None);
    }

    /// A one-page PDF drawing `text` in Helvetica with this `/Encoding`
    /// entry and a ToUnicode map from each code to `map(code)`.
    fn pdf_drawing(text: &str, encoding: &str, map: impl Fn(u8) -> u8) -> Vec<u8> {
        // Helvetica's widths for the letters drawn, 500 for the rest.
        let width = |c: u8| match c {
            b'F' => 611,
            b'f' => 278,
            b'o' | b'e' | b'g' | b'n' => 556,
            b'r' => 333,
            b'i' => 222,
            _ => 500,
        };
        let widths: Vec<String> = (65..=122u8).map(|c| width(c).to_string()).collect();
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
             1 begincodespacerange <00> <FF> endcodespacerange\n",
        );
        cmap.push_str(&format!("{} beginbfchar\n", 122 - 65 + 1));
        for c in 65..=122u8 {
            cmap.push_str(&format!("<{c:02X}> <00{:02X}>\n", map(c)));
        }
        cmap.push_str("endbfchar endcmap CMapName currentdict /CMap defineresource pop end end");
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let stream =
            |body: &str| format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len());
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] \
             /Resources << /Font << /F1 4 0 R >> >> /Contents 6 0 R >>"
                .to_string(),
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 \
                 /LastChar 122 /Widths [{}] {encoding} /ToUnicode 5 0 R >>",
                widths.join(" ")
            ),
            stream(&cmap),
            stream(&content),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    fn read(pdf: Vec<u8>) -> String {
        let doc = crate::extract_bytes(pdf).expect("a pdf");
        doc.pages[0]
            .glyphs
            .iter()
            .map(|g| g.text.as_str())
            .collect()
    }

    #[test]
    fn a_tounicode_map_that_folds_a_capital_is_read_by_the_encoding() {
        let fold = |c: u8| if c == b'F' { b'f' } else { c };
        let win_ansi = "/Encoding << /BaseEncoding /WinAnsiEncoding >>";
        // `<46> <0066>`: the F the encoding draws, 611 wide, is read as f.
        assert_eq!(read(pdf_drawing("Foreign", win_ansi, fold)), "Foreign");
        // A plain f, its own code and width, stays f.
        assert_eq!(read(pdf_drawing("foreign", win_ansi, fold)), "foreign");
        // No standard base encoding: the code's name is the font program's
        // to say, and the map's reading stands.
        assert_eq!(read(pdf_drawing("Foreign", "", fold)), "foreign");
        // A code the encoding names otherwise than one letter (`/F.sc`, a
        // small capital) keeps the map's reading.
        let small_caps = "/Encoding << /BaseEncoding /WinAnsiEncoding /Differences [70 /F.sc] >>";
        assert_eq!(read(pdf_drawing("Foreign", small_caps, fold)), "foreign");
    }

    /// A TrueType program of squares, one glyph per character given (glyph
    /// 1 for the first), its Windows Unicode cmap mapping `cmap`'s
    /// characters to those glyphs.
    fn truetype(glyphs: usize, cmap: &[(u16, u16)]) -> Vec<u8> {
        let be16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_be_bytes());
        let be32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_be_bytes());
        let n = glyphs as u16 + 1;
        // A square: one contour of four on-curve points, word deltas.
        let mut square = Vec::new();
        for x in [1u16, 100, 0, 400, 300, 3, 0] {
            be16(&mut square, x);
        }
        square.extend_from_slice(&[1, 1, 1, 1]);
        for d in [100i16, 300, 0, -300, 0, 0, 300, 0] {
            be16(&mut square, d as u16);
        }
        let mut glyf = Vec::new();
        let mut loca = vec![0u8, 0];
        for _ in 0..glyphs {
            glyf.extend_from_slice(&square);
            be16(&mut loca, (glyf.len() / 2) as u16);
        }
        loca.splice(2..2, [0u8, 0]); // .notdef is empty
        let mut head = Vec::new();
        for x in [0x0001_0000u32, 0x0001_0000, 0, 0x5F0F_3CF5] {
            be32(&mut head, x);
        }
        be16(&mut head, 0);
        be16(&mut head, 1000);
        head.extend_from_slice(&[0; 16]);
        for x in [0u16, 0, 400, 300, 0, 8, 2, 0, 0] {
            be16(&mut head, x);
        }
        let mut hhea = Vec::new();
        be32(&mut hhea, 0x0001_0000);
        for x in [
            800u16,
            (-200i16) as u16,
            0,
            500,
            0,
            0,
            400,
            1,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            n,
        ] {
            be16(&mut hhea, x);
        }
        let mut maxp = Vec::new();
        be32(&mut maxp, 0x0001_0000);
        for x in [n, 4, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
            be16(&mut maxp, x);
        }
        let mut hmtx = Vec::new();
        for _ in 0..n {
            be16(&mut hmtx, 500);
            be16(&mut hmtx, 100);
        }
        let mut post = Vec::new();
        for x in [0x0003_0000u32, 0, 0, 0, 0, 0, 0, 0] {
            be32(&mut post, x);
        }
        // cmap: one format 4 subtable, (3, 1), a segment per character.
        let mut segs: Vec<(u16, u16)> = cmap.to_vec();
        segs.sort();
        let count = segs.len() as u16 + 1;
        let mut sub = Vec::new();
        for x in [4u16, 0, 0, count * 2, 0, 0, 0] {
            be16(&mut sub, x);
        }
        for (c, _) in &segs {
            be16(&mut sub, *c);
        }
        be16(&mut sub, 0xFFFF);
        be16(&mut sub, 0);
        for (c, _) in &segs {
            be16(&mut sub, *c);
        }
        be16(&mut sub, 0xFFFF);
        for (c, g) in &segs {
            be16(&mut sub, g.wrapping_sub(*c));
        }
        be16(&mut sub, 1);
        for _ in 0..count {
            be16(&mut sub, 0);
        }
        let len = sub.len() as u16;
        sub[2..4].copy_from_slice(&len.to_be_bytes());
        let mut table_cmap = Vec::new();
        for x in [0u16, 1, 3, 1] {
            be16(&mut table_cmap, x);
        }
        be32(&mut table_cmap, 12);
        table_cmap.extend(sub);
        let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"cmap", table_cmap),
            (b"glyf", glyf),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"loca", loca),
            (b"maxp", maxp),
            (b"post", post),
        ];
        let mut font = Vec::new();
        be32(&mut font, 0x0001_0000);
        for x in [tables.len() as u16, 128, 3, 0] {
            be16(&mut font, x);
        }
        let mut offset = 12 + 16 * tables.len();
        let mut body = Vec::new();
        for (tag, data) in &mut tables {
            while data.len() % 4 != 0 {
                data.push(0);
            }
            font.extend_from_slice(*tag);
            be32(&mut font, 0);
            be32(&mut font, offset as u32);
            be32(&mut font, data.len() as u32);
            offset += data.len();
            body.extend_from_slice(data);
        }
        font.extend(body);
        font
    }

    /// The text of a page drawing CIDs 1..=glyphs in a CID TrueType font
    /// with that program, a ToUnicode map of the entries given.
    fn read_cid(program: Vec<u8>, glyphs: usize, to_unicode: &[(u16, char)]) -> String {
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
             1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
        );
        cmap.push_str(&format!("{} beginbfchar\n", to_unicode.len()));
        for (cid, c) in to_unicode {
            cmap.push_str(&format!("<{cid:04X}> <{:04X}>\n", *c as u32));
        }
        cmap.push_str("endbfchar endcmap CMapName currentdict /CMap defineresource pop end end");
        let codes: String = (1..=glyphs).map(|g| format!("{g:04X}")).collect();
        let content = format!("BT /F1 12 Tf 72 720 Td <{codes}> Tj ET");
        let stream = |dict: &str, body: &[u8]| {
            let mut s = format!("<< /Length {}{dict} >>\nstream\n", body.len()).into_bytes();
            s.extend_from_slice(body);
            s.extend_from_slice(b"\nendstream");
            s
        };
        let objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
                .to_vec(),
            b"<< /Type /Font /Subtype /Type0 /BaseFont /ABCDEF+Test /Encoding /Identity-H \
              /DescendantFonts [6 0 R] /ToUnicode 8 0 R >>"
                .to_vec(),
            stream("", content.as_bytes()),
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /ABCDEF+Test \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 500 /CIDToGIDMap /Identity /FontDescriptor 7 0 R >>"
                .to_vec(),
            format!(
                "<< /Type /FontDescriptor /FontName /ABCDEF+Test /Flags 32 \
                 /FontBBox [0 -200 500 800] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                 /CapHeight 700 /StemV 80 /FontFile2 9 0 R >>"
            )
            .into_bytes(),
            stream("", cmap.as_bytes()),
            stream(&format!(" /Length1 {}", program.len()), &program),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        read(pdf)
    }

    #[test]
    fn a_glyph_the_tounicode_map_leaves_out_is_read_by_the_programs_cmap() {
        // `B` (glyph 1) and `a` (glyph 2); the map lists `a` alone.
        let program = truetype(2, &[(0x42, 1), (0x61, 2)]);
        assert_eq!(read_cid(program.clone(), 2, &[(2, 'a')]), "Ba");
        // A map that lists the glyph stands, whatever the cmap says.
        assert_eq!(read_cid(program, 2, &[(1, 'X'), (2, 'a')]), "Xa");
    }

    #[test]
    fn a_private_use_cmap_entry_is_not_read() {
        // A symbol font's cmap maps its glyphs into the private-use area.
        let program = truetype(2, &[(0xF042, 1), (0x61, 2)]);
        assert_eq!(read_cid(program, 2, &[(2, 'a')]), "a");
    }
}
