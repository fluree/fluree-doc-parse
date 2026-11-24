//! The text a PDF states for what it draws: `/ActualText` on marked content.
//!
//! A producer that cannot map a glyph back to the character it set says so in
//! the content stream: `/Span <</ActualText (青)>> BDC … EMC` around the
//! glyph. Chrome does it for CJK text, where one glyph draws both an
//! ideograph and the radical that looks like it, and its ToUnicode map names
//! the radical (`⻘` for `青`). The glyph's ToUnicode is then not the text; the
//! span's ActualText is.
//!
//! The interpreter tells the device where marked content begins and ends,
//! with its tag and MCID, but not its properties. So the page's content
//! streams are read here first, in the order the interpreter runs them, and
//! each mark the device sees is matched to the one read. A mark whose tag or
//! MCID differs from the one expected means the two readings have parted, and
//! no ActualText is taken from that mark on.

use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::content::TypedIter;
use hayro_syntax::object::dict::keys::{
    ACTUAL_TEXT, BBOX, FORM, MCID, OC, OCG, OCMD, RESOURCES, SUBTYPE, TYPE,
};
use hayro_syntax::object::{dict_or_stream, Dict, Name, Object, String as PdfString};
use hayro_syntax::page::{Page, Resources};

/// How deep the interpreter follows forms inside forms; it skips any deeper.
const MAX_FORM_DEPTH: u32 = 50;

/// Operators read before a page's marks are taken as known. A page that
/// draws a large form hundreds of times is read that many times by the
/// interpreter too; past this the remaining marks are left unread, which
/// only means their ActualText is not taken.
const MAX_OPS: usize = 2_000_000;

/// One marked-content sequence, as the content stream begins it.
#[derive(Debug, Clone, PartialEq)]
struct Mark {
    tag: Vec<u8>,
    mcid: Option<i32>,
    actual: Option<String>,
}

/// A page's marks in the order the interpreter begins them, and how far the
/// device has got through them.
#[derive(Debug, Default)]
pub(crate) struct Marks {
    marks: Vec<Mark>,
    next: usize,
    parted: bool,
}

impl Marks {
    /// Read a page's marks, or nothing when none states ActualText: then
    /// there is nothing to match, and nothing is.
    pub(crate) fn read(page: &Page<'_>) -> Self {
        let mut scan = Scan::default();
        scan.walk(page.typed_operations(), page.resources(), 0);
        if !scan.marks.iter().any(|m| m.actual.is_some()) {
            return Self::default();
        }
        Self {
            marks: scan.marks,
            next: 0,
            parted: false,
        }
    }

    /// The device began a mark: its ActualText, when the mark is the one
    /// expected here and states one.
    pub(crate) fn begin(&mut self, tag: &[u8], mcid: Option<i32>) -> Option<String> {
        if self.parted {
            return None;
        }
        // Past the last mark read — an annotation's appearance, drawn after
        // the page — nothing is known, and nothing has parted.
        let mark = self.marks.get(self.next)?;
        self.next += 1;
        if mark.tag != tag || mark.mcid != mcid {
            self.parted = true;
            return None;
        }
        mark.actual.clone()
    }
}

#[derive(Default)]
struct Scan {
    marks: Vec<Mark>,
    ops: usize,
    /// Set where the interpreter may skip a form the scan cannot tell it
    /// runs; nothing after that point is recorded.
    stopped: bool,
}

impl Scan {
    fn walk(&mut self, mut ops: TypedIter<'_>, resources: &Resources<'_>, depth: u32) {
        // Whether each open mark makes its content optional, so a form drawn
        // inside it may not be run.
        let mut open: Vec<bool> = Vec::new();
        while let Some(op) = ops.next() {
            if self.stopped {
                return;
            }
            self.ops += 1;
            if self.ops > MAX_OPS {
                self.stopped = true;
                return;
            }
            match op {
                TypedInstruction::BeginMarkedContentWithProperties(bdc) => {
                    let props: &Object<'_> = bdc.1;
                    let inline = dict_or_stream(props).map(|(d, _)| d.clone());
                    let named = props
                        .clone()
                        .into_name()
                        .and_then(|n| resources.properties.get::<Dict<'_>>(n.as_ref()));
                    // The interpreter reads an MCID only from inline
                    // properties; so does this, or the two would disagree.
                    let mcid = inline.as_ref().and_then(|d| d.get::<i32>(MCID));
                    let actual = inline
                        .as_ref()
                        .or(named.as_ref())
                        .and_then(|d| d.get::<PdfString<'_>>(ACTUAL_TEXT))
                        .map(|s| crate::outline::decode_text_string(s.as_bytes()));
                    let optional = inline.as_ref().is_some_and(|d| d.contains_key(OC))
                        || named.as_ref().is_some_and(is_optional_content);
                    open.push(optional);
                    self.marks.push(Mark {
                        tag: bdc.0.as_ref().to_vec(),
                        mcid,
                        actual,
                    });
                }
                TypedInstruction::BeginMarkedContent(bmc) => {
                    open.push(false);
                    self.marks.push(Mark {
                        tag: bmc.0.as_ref().to_vec(),
                        mcid: None,
                        actual: None,
                    });
                }
                TypedInstruction::EndMarkedContent(_) => {
                    open.pop();
                }
                TypedInstruction::XObject(x) => {
                    self.form(x.0, resources, depth, open.contains(&true));
                }
                _ => {}
            }
        }
    }

    /// Read a form the content draws, as the interpreter would run it.
    fn form(&mut self, name: &Name<'_>, resources: &Resources<'_>, depth: u32, optional: bool) {
        let Some(stream) = resources.get_x_object(name) else {
            return;
        };
        let dict = stream.dict();
        if dict.get::<Name<'_>>(SUBTYPE).as_deref() != Some(FORM) {
            return;
        }
        // The interpreter skips a form it cannot decode or that has no
        // bounding box, and one nested too deep.
        let Ok(decoded) = stream.decoded() else {
            return;
        };
        if dict.get::<[f32; 4]>(BBOX).is_none() || depth >= MAX_FORM_DEPTH {
            return;
        }
        // Whether optional content is visible is the interpreter's to say,
        // and it skips the form when not. Unable to tell, stop here.
        if optional || dict.contains_key(OC) {
            self.stopped = true;
            return;
        }
        let own = dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default();
        let inner = Resources::from_parent(own, resources.clone());
        self.walk(TypedIter::new(decoded.as_ref()), &inner, depth + 1);
    }
}

/// An optional content group or membership dictionary.
fn is_optional_content(d: &Dict<'_>) -> bool {
    matches!(d.get::<Name<'_>>(TYPE).as_deref(), Some(t) if t == OCG || t == OCMD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(list: &[(&str, Option<i32>, Option<&str>)]) -> Marks {
        Marks {
            marks: list
                .iter()
                .map(|(t, m, a)| Mark {
                    tag: t.as_bytes().to_vec(),
                    mcid: *m,
                    actual: a.map(str::to_string),
                })
                .collect(),
            next: 0,
            parted: false,
        }
    }

    #[test]
    fn marks_are_matched_in_order() {
        let mut m = marks(&[("P", Some(0), None), ("Span", None, Some("青"))]);
        assert_eq!(m.begin(b"P", Some(0)), None);
        assert_eq!(m.begin(b"Span", None).as_deref(), Some("青"));
        // Past the end: nothing known.
        assert_eq!(m.begin(b"Span", None), None);
    }

    #[test]
    fn a_mark_out_of_step_stops_the_matching() {
        let mut m = marks(&[("Span", None, Some("a")), ("Span", None, Some("b"))]);
        assert_eq!(m.begin(b"P", Some(3)), None);
        // In step again by position, but the readings have parted once and
        // are not trusted after.
        assert_eq!(m.begin(b"Span", None), None);
    }
}
