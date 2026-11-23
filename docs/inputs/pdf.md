# PDF

The geometric path. A PDF declares no structure — it places glyphs — so
paragraphs, headings, lists, tables, columns and reading order are all
inferred from position. See [the pipeline](../design/pipeline.md) for how.

## What you get that no other source gives

- **Bounding boxes** on every element, in PDF user units with a top-left
  origin — directly usable as CSS.
- **Character-level geometry**, which is what makes [entity
  overlay](../integration/entity-overlay.md) work: a char offset resolves to a
  glyph, and a glyph range to merged per-line rectangles.
- **Per-glyph rotation**, derived from the text transform. A 90° label is an
  axis title or a vertical dimension, not body text, so orientation buckets
  reading order before it runs. This is what keeps engineering drawings
  readable and why they [do not
  escalate](../concepts/escalation.md#where-escalation-is-wrong).
- **AcroForm fields** — see [`fdoc forms`](../cli/forms.md).
- **The outline (bookmark) tree** — a near-ground-truth heading signal, and
  one no other engine tested uses.
- **Link annotations.** A hyperlink is a rectangle and an address beside the
  content stream, so a glyph pass sees the anchor's words and never what they
  point at. They are read, matched to the words they cover, and emitted in
  every format that has room for them — see
  [Links](../formats/markdown.md#links).
- **The document information dictionary** — `/Title`, `/Author`,
  `/CreationDate` and `/ModDate` — on the [DoCO](../formats/doco.md)
  document node as `dcterms:title`, `dcterms:creator`, `dcterms:created` and
  `dcterms:modified`. A date carries its offset where the file states one
  other than zero; `Z` and no offset read alike, so neither is written. What
  the file declares can be stale — a title of `Microsoft Word - draft3.doc`
  is what that file says — and a title guessed from the largest line on the
  first page would be a reading of the page, so none is.

## Text fidelity

These cleanup passes run before anything else:

**Faux-bold dedup.** PDFs fake bold by drawing text twice with a small offset.
Left alone this produces `検検討討会会のの構構成成` instead of
`検討会の構成`. A glyph drawn again where the same glyph already stands is
dropped however far apart the two draws are: a document printed with its
highlighting draws each line once under the highlight and once over it, and
read twice, every line came out with each letter doubled: `RReeccoorrdd`.

**Underlines struck over text.** A line printer underlines by printing the
line, then printing `_` under the letters it stresses. Read by position, the
two interleave: `_D_O_ _N_O_T`. A run of underscores struck over another
mark is dropped, and the words read whole. Underscores struck over nothing
are a blank left to be filled in, and stay.

**Tables drawn with characters.** The same printers draw a table's ruling
with `|` (or `¦`) down each column's edge and a line of `_` or `-` between
rows. Those characters are read back into the ruling they draw and leave the
text, so the table is read like any ruled table, header row and all. A blank
printed line inside a row parts it where the form means it to, so a step's
instruction sits beside its result blank, `Weight:` beside `________ kg`,
under the step's number.

**Highlighting.** A reviewer's highlighting is drawn as a filled band fitted
to each line it marks. Bands like that are not structure, so they are set
aside before tables and figures are looked for: read as structure, a page of
marked-up prose looked like a bar chart, or like a table of its numbered
clauses.

**Glyphs mapped to nothing.** Some producers map a glyph to U+FFFD, the
character for "unknown", and every reader of the text layer then reads
`d’a�aires` where the page prints `d’affaires`. Two such glyphs are read
from their shape instead:

- A ligature (`ff`, `fi`, `fl`, `ffi`, `ffl`), often set in a font of its
  own that keeps no names or character map, is compared with the same
  family's `f`, `i` and `l` as the document draws them elsewhere, set
  together. It is named after the ligature it matches best, when that match
  is close and clearly better than any other; otherwise it is left unknown.
- A small round mark resting on the baseline is a full stop: the dot
  leaders between a table's labels and its figures, a decimal point, the
  stops in `U.S.`.

**Spaces drawn out of place.** A space a PDF encodes is taken as a word
boundary, since it is better evidence than the width of a gap. Some
producers, though, draw a space somewhere other than where it belongs. A
page printed from a browser can carry the space from each line wrap at a
position inside an earlier word of that line, and read in place it splits
the word: `Pro duits`, `Septembr e 2026`. A space drawn out of sequence
with the letters on either side of it, and set on a letter rather than
between two, is ignored, and the word reads whole. A space drawn in
sequence, where the pen stopped after the word before it, is always kept,
however tightly the next word follows.

**Note marks.** A note's mark is drawn as a smaller glyph raised above the
baseline, right against the word it marks, and the text layer joins the two:
`Acrylonitrilea`, `Flarec (50100410)`. A raised mark after a word is read
apart from it, `Acrylonitrile a`, by the same rule as an HTML `<sup>`: a
letter or symbol after a word, or a number after a word too long to be a
unit. Raised text that belongs to its word stays in it: a unit's power
(`cm3`), an exponent (`10⁶`, `2ᵖ`), an ordinal's ending (`1st`) and a
formula's index (`ρwLv²`, `Vol(∂Ω)ⁿ`).

**NFKC normalization.** Ligatures arrive as single codepoints; searching
un-normalized output for `profile` finds one occurrence in six. Normalization
can change string length, so the engine keeps a bijective map between raw
glyph offsets and normalized text offsets. Superscripts and subscripts are
kept as printed (`m³/h`, `H₂O`) rather than folded to plain digits.

Measured Unicode resolution: **99.97%** on a 200-document Latin corpus,
**99.73%** on CJK, with **zero** replacement characters on Japanese and
Chinese government documents.

## When PDF is hard

Two failure modes, both detected rather than guessed:

- **The text is pixels.** A scanned page has no glyphs to analyze.
- **The text is garbage.** Broken CID fonts yield glyphs whose Unicode is
  unknown or wrong — the layout can be perfect and the text worthless.

`fdoc triage` reports both. See [the router](../design/router.md).

## Encrypted and malformed files

`extract_file` returns `ExtractError::Parse` for a PDF that cannot be opened.
A panic on real input is treated as a release blocker rather than a bug
report, so malformed files fail as errors you can catch.

## Performance

~7 ms per document for the deterministic tier, ~2.4 ms per page for parsing
alone, ~420 pages/s. There is no model to load, so the first document costs
what the thousandth does.
