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
    BASE_FONT, DESCENDANT_FONTS, FONT, FONT_DESC, FONT_WEIGHT, FORM, RESOURCES, STEM_V, SUBTYPE,
    XOBJECT,
};
use hayro_syntax::object::{Array, Dict, Name, Stream};
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

    fn fonts(&mut self, fonts: &Dict<'_>) {
        if !self.read.insert(fonts.cache_key()) {
            return;
        }
        for name in fonts.keys() {
            if let Some(font) = fonts.get::<Dict<'_>>(name.as_ref()) {
                self.faces
                    .entry(font.cache_key())
                    .or_insert_with(|| face(&font));
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
}
