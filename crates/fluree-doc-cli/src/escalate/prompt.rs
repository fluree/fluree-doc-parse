//! What the deep reader is asked, and why each sentence is there.
//!
//! Every rule below was put in by a measurement, and the comments say which.
//! The same text drives `eval/llm-tier/run_tier.py`, which produced the
//! published cache; if one changes and the other does not, the committed
//! scores stop describing this binary.

use crate::escalate::render::Crop;

/// A table crop: markup, because a grid is the whole content.
const TABLE: &str = "This image is one table cropped from a document page.

Transcribe it as a single HTML table.

Requirements:
- Every row and every column that is printed, in the order printed.
- Transcribe values exactly as printed, including currency symbols, commas,
  decimals, percent signs, and parentheses for negatives.
- NEVER infer, compute, complete or correct a value. If a cell is blank in
  the image, emit an empty cell. If a value is unreadable, emit an empty cell.
- Merged cells: use rowspan / colspan.
- Use <th> for header cells, <td> for data cells.

Respond with the table markup only, starting with <table> and nothing else.";

/// A routed region is whatever the text layer could not read: a chart, a
/// scanned paragraph, or a table pasted in as a picture. The router does not
/// know which, so the prompt must not assume. Told to transcribe a region
/// "one item per line", the model flattened image-tables into lines and their
/// score went to zero — the content was all there and the shape was gone.
/// The choice is made per grid rather than per image, because a crop holding
/// a caption above a table answers "not a table" for the caption's sake and
/// loses the grid with it.
const REGION: &str = "Transcribe what is printed in this image, exactly as printed,
in the order printed: top to bottom, then left to right.

Give each thing in the image the form that fits it.

- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column. It is still a table when it has no ruling lines, when
  it runs to many rows, and when it repeats its headers side by side to form
  a second pair of columns.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- A caption or note printed beside the table or chart becomes a plain text
  line, in the position it is printed.

In every case:
- This image is a crop of a larger page. Skip any line that runs off its left
  or right edge, and any line the top or bottom edge cuts through so that the
  letters are only part-height. That text belongs to something outside the
  crop and is transcribed elsewhere; transcribing it here duplicates it.
- Copy text exactly, including punctuation and decimal marks as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed in the image, return nothing at all.
- Do not describe the image and do not add commentary.";

/// Prepended when the layout detector boxed a table inside a region crop. It
/// says what the image holds; the form rules still decide how to write it.
const TABLE_HINT: &str = "This image contains a table: values arranged in rows and columns.
Transcribe that table as HTML markup, and any text printed outside it as plain
lines.

";

/// A whole page is not a big region. A region is a fragment spliced into a
/// document that already has a shape; a page *is* the shape, so its reading
/// has to carry headings and reading order. Two differences from the region
/// prompt do that, both measured: Markdown, so headings have somewhere to go
/// (a page reading arrives as one block and only a heading marker survives
/// the splice), and columns named explicitly, because "top to bottom, then
/// left to right" describes a zip for a two-column page and the model
/// followed it exactly, interleaving 1,6,2,7,3,8.
const FULL: &str = "Transcribe this page exactly as printed, as Markdown.

Reading order follows the page's own layout. Where the page is laid out in
columns or panels, read each column to its end before starting the next; do
not read straight across the page.

Mark structure as the page marks it:

- A heading -- a line set apart by size, weight, colour, or its own banner --
  becomes a Markdown heading, `#` for the most prominent rank and `##`, `###`
  below it.
- A bulleted or numbered list becomes Markdown list items.
- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- Everything else is a paragraph.

In every case:
- Copy text exactly, including punctuation and dashes as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed on the page, return nothing at all.
- Do not describe the page and do not add commentary.";

/// A link is the one piece of content a picture of a page cannot carry. The
/// anchor is visible — coloured, underlined — and the address is not, so a
/// model asked to transcribe the page sees that something is a link and has
/// nothing to make it out of. It supplies one, and an invented URL is the
/// worst kind of wrong answer because nothing downstream can tell it from a
/// real one. Where the file states the addresses, they are given here and the
/// prohibition above covers whatever is left.
fn links_hint(listing: &str) -> String {
    format!(
        "This image contains links. Their addresses are not printed on the\n\
         page, so you cannot read them from the image; they are given here:\n\n\
         {listing}\n\n\
         Where you transcribe one of those texts, write it as a Markdown link:\n\
         [text](address), using the address exactly as given. Any other text,\n\
         however it is styled, is not a link.\n\n"
    )
}

/// The prompt for one crop.
///
/// `table_boxed` is the layout detector's opinion that this region holds a
/// grid; `links` are the anchors and targets the file states inside the crop.
pub(crate) fn for_crop(crop: &Crop, table_boxed: bool, links: &[(String, String)]) -> String {
    if crop.is_table() {
        // Table markup has no place to put a Markdown link, so the listing is
        // not offered here.
        return TABLE.to_string();
    }
    let base = if crop.is_page() {
        FULL.to_string()
    } else {
        format!("{}{REGION}", if table_boxed { TABLE_HINT } else { "" })
    };
    if links.is_empty() {
        return base;
    }
    let listing = links
        .iter()
        .map(|(text, target)| format!("  \"{text}\" links to {target}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}{base}", links_hint(&listing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crop(name: &str) -> Crop {
        Crop {
            name: name.into(),
            page: 0,
            bbox: None,
            png: Vec::new(),
        }
    }

    #[test]
    fn each_crop_kind_gets_its_own_prompt() {
        assert!(for_crop(&crop("p0_t0"), true, &[]).starts_with("This image is one table"));
        assert!(for_crop(&crop("p0_full"), false, &[]).starts_with("Transcribe this page"));
        assert!(for_crop(&crop("p0_r0"), false, &[]).starts_with("Transcribe what is printed"));
    }

    #[test]
    fn a_boxed_region_is_told_it_holds_a_table() {
        assert!(for_crop(&crop("p0_r0"), true, &[]).starts_with("This image contains a table"));
    }

    #[test]
    fn known_links_are_listed_and_tables_are_not_offered_them() {
        let links = vec![("the filing".into(), "https://sec.example/x".into())];
        let p = for_crop(&crop("p0_full"), false, &links);
        assert!(p.contains("\"the filing\" links to https://sec.example/x"));
        assert!(!for_crop(&crop("p0_t0"), false, &links).contains("sec.example"));
    }

    #[test]
    fn every_prose_prompt_forbids_inventing_a_link() {
        for c in ["p0_full", "p0_r0"] {
            assert!(for_crop(&crop(c), false, &[]).contains("NEVER invent a link"));
        }
    }
}
