//! Word's list numbering, computed.
//!
//! A clause numbered `9.1.5.` by Word's list numbering holds no number in
//! its text: Word computes it when it lays the page out, from the paragraph's
//! list and level and from every paragraph of that list before it. Read as
//! text alone, the number is gone, and with it the way a reader cites the
//! clause. This module computes it the way Word does, so the reader can put
//! the label at the head of the paragraph where Word shows it.
//!
//! A paragraph is in a list through its own `w:numPr` or through its style's,
//! a style inheriting it from the style it is based on. The list (`w:num`)
//! names an abstract numbering, whose levels (`w:lvl`) give each level's
//! format, its label pattern (`%1.%2.`) and where it starts. Counters belong
//! to the abstract numbering, so two lists drawn from one carry on from each
//! other, unless a list overrides where a level starts. A level's counter
//! restarts when a shallower level advances, unless `w:lvlRestart` says
//! otherwise.

use fluree_doc_ooxml::{attr, local};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};

/// The deepest level Word numbers: `w:ilvl` runs 0 to 8.
const LEVELS: usize = 9;

/// One level of a numbering definition.
#[derive(Debug, Clone, Default)]
struct Level {
    start: u32,
    format: String,
    text: String,
    /// `w:lvlRestart`: restart after the given (1-based) level is used; 0
    /// never restarts. Absent, any shallower level restarts it.
    restart: Option<u32>,
    /// `w:isLgl`: every level's number in this level's label is shown in
    /// decimal, as legal numbering does (`1.1.1` under `I`, `A`).
    legal: bool,
    /// `w:suff`: what follows the label; Word's default is a tab.
    suffix_nothing: bool,
    /// `w:pStyle`: the paragraph style this level is linked to.
    style: Option<String>,
}

/// A list: the abstract numbering it draws on, and the levels it overrides.
#[derive(Debug, Clone, Default)]
struct Num {
    abstract_id: String,
    /// `w:lvlOverride/w:startOverride`: where a level starts in this list.
    starts: HashMap<usize, u32>,
    /// `w:lvlOverride/w:lvl`: a level this list defines for itself.
    levels: HashMap<usize, Level>,
}

/// What a paragraph style says about numbering, as written on the style.
#[derive(Debug, Clone, Default)]
struct StyleNum {
    num_id: Option<String>,
    ilvl: Option<usize>,
    based_on: Option<String>,
}

/// A document's numbering definitions, and the counters a reading keeps.
#[derive(Debug, Default)]
pub struct Numbering {
    abstracts: HashMap<String, Vec<Option<Level>>>,
    nums: HashMap<String, Num>,
    styles: HashMap<String, StyleNum>,
    counters: HashMap<String, [Option<u32>; LEVELS]>,
    started: HashSet<String>,
}

impl Numbering {
    /// Read `numbering.xml` and the numbering `styles.xml` attaches to
    /// paragraph styles. Either may be absent; a document without numbering
    /// numbers nothing.
    pub fn new(numbering_xml: Option<&str>, styles_xml: Option<&str>) -> Self {
        let mut n = Numbering::default();
        if let Some(xml) = numbering_xml {
            n.read_numbering(xml);
        }
        if let Some(xml) = styles_xml {
            n.styles = read_style_numbering(xml);
        }
        n
    }

    fn read_numbering(&mut self, xml: &str) {
        let mut r = Reader::from_str(xml);
        let mut buf = Vec::new();
        // Where a `w:lvl` being read belongs: an abstract numbering, or a
        // list's override.
        let mut abstract_id: Option<String> = None;
        let mut num_id: Option<String> = None;
        let mut override_lvl: Option<usize> = None;
        let mut lvl: Option<(usize, Level)> = None;
        loop {
            match r.read_event_into(&mut buf) {
                Err(_) | Ok(Event::Eof) => break,
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                    "abstractNum" => {
                        abstract_id = attr(&e, "abstractNumId");
                    }
                    "num" if abstract_id.is_none() => {
                        num_id = attr(&e, "numId");
                        if let Some(id) = &num_id {
                            self.nums.entry(id.clone()).or_default();
                        }
                    }
                    "abstractNumId" => {
                        if let (Some(id), Some(v)) = (&num_id, attr(&e, "val")) {
                            self.nums.entry(id.clone()).or_default().abstract_id = v;
                        }
                    }
                    "lvlOverride" => {
                        override_lvl = attr(&e, "ilvl").and_then(|v| v.parse().ok());
                    }
                    "startOverride" => {
                        if let (Some(id), Some(l), Some(v)) = (
                            &num_id,
                            override_lvl,
                            attr(&e, "val").and_then(|v| v.parse().ok()),
                        ) {
                            self.nums.entry(id.clone()).or_default().starts.insert(l, v);
                        }
                    }
                    "lvl" => {
                        let ilvl = attr(&e, "ilvl")
                            .and_then(|v| v.parse::<usize>().ok())
                            .or(override_lvl)
                            .unwrap_or(0);
                        lvl = Some((
                            ilvl,
                            Level {
                                start: 1,
                                format: "decimal".into(),
                                ..Level::default()
                            },
                        ));
                    }
                    tag => {
                        if let Some((_, l)) = lvl.as_mut() {
                            let v = attr(&e, "val");
                            match tag {
                                "start" => l.start = v.and_then(|v| v.parse().ok()).unwrap_or(1),
                                "numFmt" => l.format = v.unwrap_or_else(|| "decimal".into()),
                                "lvlText" => l.text = v.unwrap_or_default(),
                                "lvlRestart" => l.restart = v.and_then(|v| v.parse().ok()),
                                "isLgl" => {
                                    l.legal = !matches!(v.as_deref(), Some("0" | "false" | "off"))
                                }
                                "suff" => l.suffix_nothing = v.as_deref() == Some("nothing"),
                                "pStyle" => l.style = v,
                                _ => {}
                            }
                        }
                    }
                },
                Ok(Event::End(e)) => match local(e.name().as_ref()) {
                    "lvl" => {
                        if let Some((ilvl, level)) = lvl.take() {
                            if ilvl < LEVELS {
                                if let Some(id) = &abstract_id {
                                    let levels = self
                                        .abstracts
                                        .entry(id.clone())
                                        .or_insert_with(|| vec![None; LEVELS]);
                                    levels[ilvl] = Some(level);
                                } else if let Some(id) = &num_id {
                                    self.nums
                                        .entry(id.clone())
                                        .or_default()
                                        .levels
                                        .insert(ilvl, level);
                                }
                            }
                        }
                    }
                    "abstractNum" => abstract_id = None,
                    "num" => num_id = None,
                    "lvlOverride" => override_lvl = None,
                    _ => {}
                },
                _ => {}
            }
            buf.clear();
        }
    }

    /// The list and level a paragraph is numbered in, from its own
    /// `w:numPr` (either part of which may be missing) and its style's.
    ///
    /// The style's numbering is followed up the styles it is based on. A
    /// style that names its list but no level takes the level linked to it
    /// (`w:lvl/w:pStyle`), as Word does for outline headings. `numId` 0
    /// switches numbering off.
    pub fn resolve(
        &self,
        num_id: Option<&str>,
        ilvl: Option<usize>,
        style: Option<&str>,
    ) -> Option<(String, usize)> {
        let (mut style_num, mut style_lvl) = (None, None);
        let mut at = style.map(str::to_string);
        for _ in 0..16 {
            let Some(s) = at.as_ref().and_then(|s| self.styles.get(s)) else {
                break;
            };
            style_num = style_num.or(s.num_id.clone());
            style_lvl = style_lvl.or(s.ilvl);
            at = s.based_on.clone();
        }
        let id = num_id.map(str::to_string).or(style_num)?;
        if id == "0" {
            return None;
        }
        let level = ilvl.or(style_lvl).or_else(|| {
            let style = style?;
            let num = self.nums.get(&id)?;
            self.abstracts
                .get(&num.abstract_id)?
                .iter()
                .position(|l| l.as_ref().and_then(|l| l.style.as_deref()) == Some(style))
        });
        Some((id, level.unwrap_or(0).min(LEVELS - 1)))
    }

    fn level(&self, num: &Num, ilvl: usize) -> Option<Level> {
        num.levels.get(&ilvl).cloned().or_else(|| {
            self.abstracts
                .get(&num.abstract_id)
                .and_then(|l| l.get(ilvl).cloned().flatten())
        })
    }

    /// Advance the counters for a paragraph numbered in list `num_id` at
    /// `ilvl`, and give its label as Word shows it (`9.1.5.`), with what
    /// follows the label: a space, or nothing where the level says so.
    /// `None` for a bullet, a level with no label, or a list the document
    /// does not define. The counters advance for every numbered paragraph,
    /// shown or not, as Word's do.
    pub fn next(&mut self, num_id: &str, ilvl: usize) -> Option<String> {
        let num = self.nums.get(num_id)?.clone();
        let levels: Vec<Option<Level>> = (0..LEVELS).map(|k| self.level(&num, k)).collect();
        let level = levels[ilvl].clone()?;
        let counters = self
            .counters
            .entry(num.abstract_id.clone())
            .or_insert([None; LEVELS]);
        if !num.starts.is_empty() && self.started.insert(num_id.to_string()) {
            for (&k, &s) in &num.starts {
                if k < LEVELS {
                    counters[k] = Some(s.saturating_sub(1));
                }
            }
        }
        counters[ilvl] = Some(counters[ilvl].map_or(level.start, |n| n + 1));
        for (d, slot) in counters.iter_mut().enumerate().skip(ilvl + 1) {
            let restarts = match levels[d].as_ref().and_then(|l| l.restart) {
                Some(0) => false,
                Some(after) => (ilvl as u32) < after,
                None => true,
            };
            if restarts {
                *slot = None;
            }
        }
        if matches!(level.format.as_str(), "bullet" | "none") {
            return None;
        }
        let counters = *counters;
        let mut label = String::new();
        let mut chars = level.text.chars().peekable();
        while let Some(c) = chars.next() {
            let k = match (c, chars.peek()) {
                ('%', Some(d)) if ('1'..='9').contains(d) => *d as usize - '1' as usize,
                _ => {
                    label.push(c);
                    continue;
                }
            };
            chars.next();
            let of = levels[k].as_ref();
            let n = counters[k].unwrap_or_else(|| of.map_or(1, |l| l.start));
            let format = if level.legal {
                "decimal"
            } else {
                of.map_or("decimal", |l| l.format.as_str())
            };
            label.push_str(&format_number(n, format));
        }
        let label = label.trim().to_string();
        if label.is_empty() {
            return None;
        }
        Some(if level.suffix_nothing {
            label
        } else {
            label + " "
        })
    }
}

/// Paragraph styles' own numbering properties, and the styles they are
/// based on.
fn read_style_numbering(xml: &str) -> HashMap<String, StyleNum> {
    let mut r = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut out: HashMap<String, StyleNum> = HashMap::new();
    let mut style: Option<String> = None;
    let mut in_ppr = false;
    loop {
        match r.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match local(e.name().as_ref()) {
                "style" => {
                    style = (attr(&e, "type").as_deref() == Some("paragraph"))
                        .then(|| attr(&e, "styleId"))
                        .flatten();
                }
                "pPr" => in_ppr = true,
                "basedOn" => {
                    if let (Some(s), Some(v)) = (&style, attr(&e, "val")) {
                        out.entry(s.clone()).or_default().based_on = Some(v);
                    }
                }
                "numId" if in_ppr => {
                    if let (Some(s), Some(v)) = (&style, attr(&e, "val")) {
                        out.entry(s.clone()).or_default().num_id = Some(v);
                    }
                }
                "ilvl" if in_ppr => {
                    if let (Some(s), Some(v)) =
                        (&style, attr(&e, "val").and_then(|v| v.parse().ok()))
                    {
                        out.entry(s.clone()).or_default().ilvl = Some(v);
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                "style" => style = None,
                "pPr" => in_ppr = false,
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    out
}

/// A counter's value in one of Word's number formats. Formats without a
/// rule here (`ordinal`, `cardinalText`, the East Asian ones) are written in
/// decimal, which keeps the number if not its spelling.
fn format_number(n: u32, format: &str) -> String {
    match format {
        "lowerLetter" | "upperLetter" => {
            // Word repeats the letter past z: aa, bb, … not Excel's aa, ab.
            let n = n.max(1);
            let letter = (b'a' + ((n - 1) % 26) as u8) as char;
            let s: String = std::iter::repeat_n(letter, ((n - 1) / 26 + 1) as usize).collect();
            if format == "upperLetter" {
                s.to_uppercase()
            } else {
                s
            }
        }
        "lowerRoman" => roman(n),
        "upperRoman" => roman(n).to_uppercase(),
        "decimalZero" if n < 10 => format!("0{n}"),
        _ => n.to_string(),
    }
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
