//! TeX's Computer Modern fonts, read by TeX's own layouts.
//!
//! TeX's fonts place their glyphs by fixed layouts, one for each family:
//! the text fonts (`cmr`, `cmbx`) by OT1, the math italic (`cmmi`) by OML,
//! the math symbols (`cmsy`) by OMS and the math extension (`cmex`) by
//! OMX. In the math italic, code 61 is a slash and 58 a period; in the
//! symbols, 2 is × and 21 is ≥.
//!
//! pdfTeX embeds the fonts with names that say so (`/slash` at 61), and
//! they read as named. Some typesetters embed them under a generic vector
//! instead (`TeX_CM_Maths_Italic`, `TeX_CM_Maths_Symbols`): `C0`–`C31` at
//! codes 0–31 and the ASCII names at their ASCII codes, `/equal` at 61,
//! `/colon` at 58. The glyphs stay where TeX's layout has them, and the
//! ToUnicode map repeats the vector, so `t1/2` read `t 1=2`, `0.0044` read
//! `0:0044` and × read as nothing. Every reader that follows the names
//! reads them so.
//!
//! A font is read by its family's layout where:
//! - its name says which family it is ([`layout_of`]);
//! - its program's vector is its own, not a predefined one;
//! - the vector labels glyphs by their codes ([`labelled_by_codes`]).
//!
//! A glyph's place in the layout is its code in that vector. A ToUnicode
//! map that says anything other than the label stands
//! ([`crate::faces::Faces::tex`]).

/// One of TeX's 7-bit font layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    Ot1,
    Oml,
    Oms,
    Omx,
}

/// The layout a font's name says it is set in: one of TeX's Computer Modern
/// families by its own name (`CMMI10`, `CMSY7`, `CMEX10`, `CMR10`,
/// `CMBX12`), a subset's tag before it or not, or by the names some
/// typesetters give them (`TeX_CM_Maths_Italic`). The typewriter (`cmtt`)
/// and other text faces with layouts of their own are none.
pub(crate) fn layout_of(name: &str) -> Option<Layout> {
    let name = name.split_once('+').map_or(name, |(_, n)| n);
    let n: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    for (prefix, layout) in [
        ("texcmmathsitalic", Layout::Oml),
        ("texcmmathssymbols", Layout::Oms),
        ("texcmmathsextension", Layout::Omx),
        ("texcmroman", Layout::Ot1),
    ] {
        if n.starts_with(prefix) {
            return Some(layout);
        }
    }
    let stem = n.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() == n.len() || stem.is_empty() {
        return None;
    }
    match stem {
        "cmr" | "cmbx" => Some(Layout::Ot1),
        "cmmi" | "cmmib" => Some(Layout::Oml),
        "cmsy" | "cmbsy" => Some(Layout::Oms),
        "cmex" => Some(Layout::Omx),
        _ => None,
    }
}

/// The text the glyph at `code` in a layout is.
pub(crate) fn text(layout: Layout, code: u8) -> Option<&'static str> {
    let table = match layout {
        Layout::Ot1 => &OT1,
        Layout::Oml => &OML,
        Layout::Oms => &OMS,
        Layout::Omx => &OMX,
    };
    table.get(code as usize).copied()
}

/// Whether a vector labels its glyphs by their codes: every code from 33
/// to 126 it names reads, by the glyph list, as the ASCII character of
/// that code, or as the curly quote StandardEncoding names at 39 and 96;
/// a name the list does not read (`prime`) is not a code's label. At least
/// one must be named. A vector with TeX's own names, `/slash` at
/// 61 in a math italic or `/quotedblright` at 34 in a text font, does not.
pub(crate) fn labelled_by_codes<'n>(name: impl Fn(u8) -> Option<&'n str>) -> bool {
    let mut named = 0;
    for code in 33u8..=126 {
        let Some(n) = name(code).filter(|n| *n != ".notdef") else {
            continue;
        };
        let read: String = read_fonts::ps::agl::name_to_chars(n).collect();
        let curly = match code {
            39 => Some("’"),
            96 => Some("‘"),
            _ => None,
        };
        if read != (code as char).to_string() && Some(read.as_str()) != curly {
            return false;
        }
        named += 1;
    }
    named > 0
}

/// TeX's text layout, OT1 (`cmr`, `cmbx`).
const OT1: [&str; 128] = [
    "Γ", "Δ", "Θ", "Λ", "Ξ", "Π", "Σ", "Υ", "Φ", "Ψ", "Ω", "ff", "fi", "fl", "ffi", "ffl", "ı",
    "ȷ", "`", "´", "ˇ", "˘", "¯", "˚", "¸", "ß", "æ", "œ", "ø", "Æ", "Œ", "Ø", "", "!", "”", "#",
    "$", "%", "&", "’", "(", ")", "*", "+", ",", "-", ".", "/", "0", "1", "2", "3", "4", "5", "6",
    "7", "8", "9", ":", ";", "¡", "=", "¿", "?", "@", "A", "B", "C", "D", "E", "F", "G", "H", "I",
    "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "[", "“",
    "]", "ˆ", "˙", "‘", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o",
    "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "–", "—", "˝", "˜", "¨",
];

/// TeX's math italic layout, OML (`cmmi`).
const OML: [&str; 128] = [
    "Γ", "Δ", "Θ", "Λ", "Ξ", "Π", "Σ", "Υ", "Φ", "Ψ", "Ω", "α", "β", "γ", "δ", "ϵ", "ζ", "η", "θ",
    "ι", "κ", "λ", "μ", "ν", "ξ", "π", "ρ", "σ", "τ", "υ", "ϕ", "χ", "ψ", "ω", "ε", "ϑ", "ϖ", "ϱ",
    "ς", "φ", "↼", "↽", "⇀", "⇁", "", "", "▹", "◃", "0", "1", "2", "3", "4", "5", "6", "7", "8",
    "9", ".", ",", "<", "/", ">", "⋆", "∂", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K",
    "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "♭", "♮", "♯", "⌣",
    "⌢", "ℓ", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
    "r", "s", "t", "u", "v", "w", "x", "y", "z", "ı", "ȷ", "℘", "", "",
];

/// TeX's math symbols layout, OMS (`cmsy`).
const OMS: [&str; 128] = [
    "−", "·", "×", "∗", "÷", "⋄", "±", "∓", "⊕", "⊖", "⊗", "⊘", "⊙", "◯", "∘", "∙", "≍", "≡", "⊆",
    "⊇", "≤", "≥", "⪯", "⪰", "∼", "≈", "⊂", "⊃", "≪", "≫", "≺", "≻", "←", "→", "↑", "↓", "↔", "↗",
    "↘", "≃", "⇐", "⇒", "⇑", "⇓", "⇔", "↖", "↙", "∝", "′", "∞", "∈", "∋", "△", "▽", "/", "", "∀",
    "∃", "¬", "∅", "ℜ", "ℑ", "⊤", "⊥", "ℵ", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K",
    "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "∪", "∩", "⊎", "∧",
    "∨", "⊢", "⊣", "⌊", "⌋", "⌈", "⌉", "{", "}", "⟨", "⟩", "|", "‖", "↕", "⇕", "\\", "≀", "√", "⨿",
    "∇", "∫", "⊔", "⊓", "⊑", "⊒", "§", "†", "‡", "¶", "♣", "♢", "♡", "♠",
];

/// TeX's math extension layout, OMX (`cmex`): large delimiters and operators, read as the character they are a size of; a piece of a delimiter built from pieces as Unicode's piece, a piece with none as nothing.
const OMX: [&str; 128] = [
    "(", ")", "[", "]", "⌊", "⌋", "⌈", "⌉", "{", "}", "⟨", "⟩", "|", "‖", "/", "\\", "(", ")", "(",
    ")", "[", "]", "⌊", "⌋", "⌈", "⌉", "{", "}", "⟨", "⟩", "/", "\\", "(", ")", "[", "]", "⌊", "⌋",
    "⌈", "⌉", "{", "}", "⟨", "⟩", "/", "\\", "/", "\\", "⎛", "⎞", "⎡", "⎤", "⎣", "⎦", "⎢", "⎥",
    "⎧", "⎫", "⎩", "⎭", "⎨", "⎬", "⎪", "⏐", "⎝", "⎠", "⎜", "⎟", "⟨", "⟩", "⨆", "⨆", "∮", "∮", "⨀",
    "⨀", "⨁", "⨁", "⨂", "⨂", "∑", "∏", "∫", "⋃", "⋂", "⨄", "⋀", "⋁", "∑", "∏", "∫", "⋃", "⋂", "⨄",
    "⋀", "⋁", "∐", "∐", "ˆ", "ˆ", "ˆ", "˜", "˜", "˜", "[", "]", "⌊", "⌋", "⌈", "⌉", "{", "}", "√",
    "√", "√", "√", "⎷", "", "", "‖", "↑", "↓", "", "", "", "", "⇑", "⇓",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_by_their_names() {
        assert_eq!(layout_of("TeX_CM_Maths_Italic"), Some(Layout::Oml));
        assert_eq!(layout_of("TeX_CM_Maths_Symbols"), Some(Layout::Oms));
        assert_eq!(layout_of("TeX_CM_Maths_Extension"), Some(Layout::Omx));
        assert_eq!(layout_of("TeX_CM_Roman"), Some(Layout::Ot1));
        assert_eq!(layout_of("XVABQS+CMMI10"), Some(Layout::Oml));
        assert_eq!(layout_of("CMSY7"), Some(Layout::Oms));
        assert_eq!(layout_of("CMEX10"), Some(Layout::Omx));
        assert_eq!(layout_of("CMBX12"), Some(Layout::Ot1));
        // The typewriter has a layout of its own; other fonts are none.
        assert_eq!(layout_of("CMTT10"), None);
        assert_eq!(layout_of("CMMI"), None);
        assert_eq!(layout_of("CMRoman"), None);
        assert_eq!(layout_of("MinionPro-Regular"), None);
        assert_eq!(layout_of("Helvetica"), None);
    }

    #[test]
    fn the_glyphs_that_read_wrongly() {
        assert_eq!(text(Layout::Oml, 61), Some("/"));
        assert_eq!(text(Layout::Oml, 58), Some("."));
        assert_eq!(text(Layout::Oml, 59), Some(","));
        assert_eq!(text(Layout::Oms, 2), Some("×"));
        assert_eq!(text(Layout::Oms, 21), Some("≥"));
        assert_eq!(text(Layout::Omx, 14), Some("/"));
        assert_eq!(text(Layout::Ot1, 123), Some("–"));
        assert_eq!(text(Layout::Ot1, 61), Some("="));
        assert_eq!(text(Layout::Oml, 128), None);
    }

    #[test]
    fn a_vector_labelled_by_codes() {
        let generic = |c: u8| -> Option<&'static str> {
            Some(match c {
                39 => "quoteright",
                46 => "period",
                47 => "slash",
                58 => "colon",
                61 => "equal",
                97 => "a",
                _ => return None,
            })
        };
        assert!(labelled_by_codes(generic));
        // pdfTeX's math italic names its glyphs as TeX draws them.
        let tex = |c: u8| -> Option<&'static str> {
            Some(match c {
                58 => "period",
                61 => "slash",
                97 => "a",
                _ => return None,
            })
        };
        assert!(!labelled_by_codes(tex));
        // A text font's closing quote at 34.
        assert!(!labelled_by_codes(|c| (c == 34).then_some("quotedblright")));
        // A name the glyph list does not read, pdfTeX's `/prime` at 48.
        assert!(!labelled_by_codes(|c| (c == 48).then_some("prime")));
        // Nothing named from 33 to 126.
        assert!(!labelled_by_codes(|c| (c < 33).then_some("C1")));
    }

    /// Type 1's eexec and charstring encryption.
    fn encrypt(plain: &[u8], mut r: u16) -> Vec<u8> {
        plain
            .iter()
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (c as u16)
                    .wrapping_add(r)
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                c
            })
            .collect()
    }

    /// A Type 1 program named `name` whose vector puts these names at these
    /// codes, each glyph a square.
    /// With `standard`, the program's vector is StandardEncoding instead, and
    /// the names are its glyphs only.
    fn program(name: &str, vector: &[(u8, &str)], standard: bool) -> (Vec<u8>, usize) {
        let mut clear = format!(
            "%!PS-AdobeFont-1.0: {name}\n12 dict begin\n/FontName /{name} def\n/PaintType 0 def\n\
             /FontType 1 def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
             /FontBBox {{0 0 500 500}} readonly def\n"
        );
        if standard {
            clear.push_str("/Encoding StandardEncoding def\n");
        } else {
            clear.push_str("/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\n");
            for (code, glyph) in vector {
                clear.push_str(&format!("dup {code} /{glyph} put\n"));
            }
            clear.push_str("readonly def\n");
        }
        clear.push_str("currentdict end\ncurrentfile eexec\n");
        // 0 500 hsbw 100 0 rmoveto 300 0 rlineto 0 300 rlineto -300 0 rlineto closepath endchar
        let square = [
            139, 248, 136, 13, 239, 139, 21, 247, 192, 139, 5, 139, 247, 192, 5, 251, 192, 139, 5,
            9, 14,
        ];
        let mut charstring = vec![0u8; 4];
        charstring.extend_from_slice(&square);
        let charstring = encrypt(&charstring, 4330);
        let mut private = b"\0\0\0\0dup /Private 8 dict dup begin\n\
            /RD{string currentfile exch readstring pop}executeonly def\n\
            /ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n\
            /lenIV 4 def\n/Subrs 0 array ND\n"
            .to_vec();
        private.extend_from_slice(
            format!("2 index /CharStrings {} dict dup begin\n", vector.len() + 1).as_bytes(),
        );
        for glyph in std::iter::once(".notdef").chain(vector.iter().map(|(_, g)| *g)) {
            private.extend_from_slice(format!("/{glyph} {} RD ", charstring.len()).as_bytes());
            private.extend_from_slice(&charstring);
            private.extend_from_slice(b" ND\n");
        }
        private.extend_from_slice(
            b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\n\
              mark currentfile closefile\n",
        );
        let mut data = clear.into_bytes();
        let length1 = data.len();
        data.extend(encrypt(&private, 55665));
        (data, length1)
    }

    /// The text of a page drawing `text` in a font named `name`, embedded
    /// with that vector, and a ToUnicode map giving each code `map(code)`,
    /// or none.
    fn read(
        name: &str,
        vector: &[(u8, &str)],
        text: &str,
        map: Option<&dyn Fn(u8) -> char>,
    ) -> String {
        read_program(name, vector, false, text, map)
    }

    fn read_program(
        name: &str,
        vector: &[(u8, &str)],
        standard: bool,
        text: &str,
        map: Option<&dyn Fn(u8) -> char>,
    ) -> String {
        let (program, length1) = program(name, vector, standard);
        let first = vector.iter().map(|(c, _)| *c).min().unwrap();
        let last = vector.iter().map(|(c, _)| *c).max().unwrap();
        let widths = vec!["500"; (last - first + 1) as usize].join(" ");
        let to_unicode = map.map(|map| {
            let mut cmap = String::from(
                "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                 1 begincodespacerange <00> <FF> endcodespacerange\n",
            );
            cmap.push_str(&format!("{} beginbfchar\n", vector.len()));
            for (code, _) in vector {
                cmap.push_str(&format!("<{code:02X}> <{:04X}>\n", map(*code) as u32));
            }
            cmap.push_str(
                "endbfchar endcmap CMapName currentdict /CMap defineresource pop end end",
            );
            cmap.into_bytes()
        });
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET").into_bytes();
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
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /FirstChar {first} \
                 /LastChar {last} /Widths [{widths}] /FontDescriptor 6 0 R{} >>",
                if to_unicode.is_some() {
                    " /ToUnicode 8 0 R"
                } else {
                    ""
                }
            )
            .into_bytes(),
            stream("", &content),
            format!(
                "<< /Type /FontDescriptor /FontName /{name} /Flags 4 /FontBBox [0 0 500 500] \
                 /ItalicAngle 0 /Ascent 500 /Descent 0 /CapHeight 500 /StemV 80 /FontFile 7 0 R >>"
            )
            .into_bytes(),
            stream(
                &format!(
                    " /Length1 {length1} /Length2 {} /Length3 0",
                    program.len() - length1
                ),
                &program,
            ),
            stream("", to_unicode.as_deref().unwrap_or(b"")),
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
        let doc = crate::extract_bytes(pdf).expect("a pdf");
        doc.pages[0]
            .glyphs
            .iter()
            .map(|g| g.text.as_str())
            .collect()
    }

    /// The generic vector: the ASCII names at their codes, `C2` below 32.
    const GENERIC: &[(u8, &str)] = &[
        (2, "C2"),
        (49, "one"),
        (50, "two"),
        (58, "colon"),
        (59, "semicolon"),
        (61, "equal"),
        (116, "t"),
    ];

    #[test]
    fn a_math_font_labelled_by_codes_reads_as_tex_draws_it() {
        let ascii = |c: u8| if c < 32 { '\u{FFFD}' } else { c as char };
        // `t1/2`, `,`, `.` and × in TeX's math italic and symbols.
        assert_eq!(
            read("TeX_CM_Maths_Italic", GENERIC, "t1=2;:", Some(&ascii)),
            "t1/2,."
        );
        assert_eq!(
            read("TeX_CM_Maths_Symbols", GENERIC, "\\002", Some(&ascii)),
            "×"
        );
        // With no ToUnicode map, the names read the same way.
        assert_eq!(read("TeX_CM_Maths_Italic", GENERIC, "t1=2", None), "t1/2");
    }

    #[test]
    fn other_fonts_and_maps_stand() {
        let ascii = |c: u8| if c < 32 { '\u{FFFD}' } else { c as char };
        // Not a TeX font by its name.
        assert_eq!(
            read("MathFont-Italic", GENERIC, "t1=2", Some(&ascii)),
            "t1=2"
        );
        // A ToUnicode map that says otherwise than the label.
        let own = |c: u8| if c == 61 { '÷' } else { c as char };
        assert_eq!(
            read("TeX_CM_Maths_Italic", GENERIC, "1=2", Some(&own)),
            "1÷2"
        );
        // A vector that names a glyph as TeX draws it, not by its code
        // (`/quotedblright` at 34): read by its names.
        let named = &[
            (34, "quotedblright"),
            (49, "one"),
            (50, "two"),
            (61, "equal"),
        ];
        assert_eq!(read("TeX_CM_Maths_Italic", named, "1=2", None), "1=2");
        // A program reached through StandardEncoding.
        assert_eq!(
            read_program("TeX_CM_Maths_Italic", GENERIC, true, "1=2", None),
            "1=2"
        );
    }
}
