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

use std::collections::{HashMap, HashSet};

use hayro_interpret::CacheKey;
use hayro_syntax::object::dict::keys::{
    BASE_ENCODING, BASE_FONT, DESCENDANT_FONTS, DIFFERENCES, ENCODING, FIRST_CHAR, FONT, FONT_DESC,
    FONT_WEIGHT, FORM, MAC_ROMAN_ENCODING, MM_TYPE1, RESOURCES, STANDARD_ENCODING, STEM_V, SUBTYPE,
    TO_UNICODE, TRUE_TYPE, TYPE1, WIDTHS, WIN_ANSI_ENCODING, XOBJECT,
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
}

/// The faces of the fonts a document draws with, by the interpreter's key
/// for each font, gathered page by page.
#[derive(Debug, Default)]
pub(crate) struct Faces {
    faces: HashMap<u128, Face>,
    /// Letters each font's ToUnicode map gives in the wrong case, by the
    /// interpreter's key for the font ([`recased`]).
    recase: HashMap<u128, Vec<Recase>>,
    /// Resource dictionaries already read, so a form every page draws is
    /// read once.
    read: HashSet<u128>,
}

impl Faces {
    /// Read the fonts a page and the forms it draws can use.
    pub(crate) fn read(&mut self, page: &Page<'_>) {
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

    fn fonts(&mut self, fonts: &Dict<'_>) {
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
            }
        }
    }

    fn forms(&mut self, x_objects: &Dict<'_>, depth: u32) {
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
    Face {
        named,
        declared,
        stem,
    }
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
}
