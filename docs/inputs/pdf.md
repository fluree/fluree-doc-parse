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

**Watermarks.** A `DRAFT`, a diagonal `CONFIDENTIAL` or an `Uncontrolled
copy` stamp is drawn over the page's text, at the same place on every page.
Read in place, its letters land among the words of every line it crosses.
It is taken out before lines are read: a run of text that repeats at one
place across pages and lies on the text beneath it, set apart from that
text (turned, much larger, or drawn as a layer of its own), or stands in a
margin beside all of it. Text a template repeats at the same place, such as
a spec table's labels or a box's heading, sits among the page's lines and
is kept.

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

**Symbol fonts.** A PDF made from Word gives a character set in Symbol or
Wingdings the font's own code plus `0xF000` as its text, a private-use
character, and every reader of the text layer gets that code: `1600 μmole`
reads `1600 mole` with an unprintable character before `mole`, `HIi = ∑HQij` loses its
`=` and `∑`, and an arrow between two values is gone. A glyph drawn in
Symbol, Wingdings or Wingdings 2 reads as the character the font shows, by
the same table the Word reader uses: `μ`, `±`, `=`, `∑` from Symbol, `→`,
`❑`, `✔` from Wingdings. The font is known by its name in the PDF, or, where
a producer renamed its fonts (`CIDFont+F4`), by the name inside the font
program. A code in any other font is kept as it is.

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

**Lines wrapped inside a word.** Chinese and Japanese set no spaces
between words, so a line may wrap between any two characters. A line that
runs to its paragraph's edge and ends on such a character joins the next
without a space: `青` at a line's end and `松纸业有限公司` on the next read
as one name. A line that stops short was ended by hand, and the next is
another item, so a space keeps them apart. Korean spaces its words and
wraps at the spaces, and joins with one.

**Hyphens at a line's end.** A hyphen ending a line between two letters of
one case, with at least two on each side, is the break a typesetter set
inside a word, and the word reads whole: `microcom-` over `puter` is
`microcomputer`. Any other hyphen there is the text's own and stays, the
lines joined without a space: in a number or a code (`1-800-222-` over
`1222`, `EPA-530-R-92-019`), a compound with a capital or a single letter
(`Polish-German`, `t-test`), a range (`(A)-(L)`). A hyphen with a space
before it is a dash, and a space follows it too. An all-capital compound
broken at its hyphen cannot be told from an all-capital word the typesetter
broke, and reads as the word. Some producers map the hyphens they draw to
U+00AD SOFT HYPHEN; drawn, it is read as the hyphen it prints, and one that
draws nothing is dropped.

**Bold inside the text.** A heading set in bold is bold from its first word
to its last, so a line reads as bold only when it is: most of its glyphs
bold, and its first and last words. A face that declares no weight counts
as regular, since many documents declare one for their bold face only. A
bold label opening a paragraph (`PUMP: With stainless steel pump…`) is the
paragraph's run-in head, not a heading: the paragraph starts at the label
and reads whole. A line that runs to its column's edge was wrapped, not
ended, so the next line goes on with the same paragraph whatever its
weight, and bold names wrapped onto a line of their own stay in their
sentence. The next line goes on only from its column's left edge: a line
starting well to the left of it belongs to another column. A bullet at the head of a line (`•`, `■`) begins a list item,
even below a line that runs to the edge. A list with a hanging indent sets
its bullets at the margin and each line of an item's text in from them, so
an item's wrapped line starts an em or two right of the bullet line above
it: up to three ems, it goes on with that item rather than starting a
paragraph. A line that opens with a dash, or follows an item ending on a
colon, starts a list of its own under the item.

**Bullets.** The typographic bullets and geometric shapes are read as
bullets, and so are the arrows and boxes the dingbat fonts set as them
(`➢`, `◆`, `❑`). A Word file's symbol-font bullets read as the characters
they show (see Symbol fonts above): the bullet of a default bulleted list,
U+F0B7 in the text layer, is `•`, and a Wingdings box is `❑`. Where the
font's name does not say which font it is, the private-use codes of those
bullets are read as bullets all the same, but only codes that are no letter
in Symbol or Wingdings, since the same code is `❑` in one and `θ` in the
other. A block that
opens with a bullet is a list item unless it is a heading, and one that opens
with a plain bullet or a box to tick is never a heading, whatever its size
or weight: an option list's bold items (`❑ Peg rack`) stay items. An arrow
or a diamond is set before headings as often as before items (`❖ Gene` over
a guideline's subsection), so there the heading pass decides.

**Note marks.** A note's mark is drawn as a smaller glyph raised above the
baseline, right against the word it marks, and the text layer joins the two:
`Acrylonitrilea`, `Flarec (50100410)`. A raised mark after a word is read
apart from it, `Acrylonitrile a`, by the same rule as an HTML `<sup>`: a
letter or symbol after a word, or a number after a word too long to be a
unit, after a quantity (`26TB 1`, not `26TB1`), or after a bracket that
holds words (`(49 N) 5`, not `(49 N)5`). Raised text that belongs to its
word stays in it: a unit's power, kept raised (`cm³`, `m/s²`, and the
Spanish square foot `PIES²`), an ordinal's ending (`1st`) and a formula's
index or power (`ρwLv²`, `Vol(∂Ω)ⁿ`, `(a+b)²`).

After a number, the same marks matter more, because the text layer turns
them into another number. A note's letters stand apart from the value:
`110ᵉ` reads `110 e`, and `610ᵉ,ᶠ` reads `610 e,f`. Raised digits and signs
after a number keep their place as Unicode superscripts, whether a power
or a note's number: `kg/10⁶ dscm`, not `kg/106 dscm`; `4.0x10⁻³`, not
`4.0x10-3`; `2024³`, not `20243`. An ordinal's ending stays with the number
it agrees with (`22nd`, `118th`, `21e`).

A degree sign set as a raised `o` or `0` reads as `°` wherever it cannot be
an ordinal's ending or a power: before a temperature's unit (`25°C`, not
`25oC`), after three figures or more (`~ 145°`, not `~ 145 o`), after a
decimal, or after a signed number. `1º` and `2º` stay ordinals and `10⁰` a
power.

**Mixed numbers.** A spec sheet sets `22 3/8"` with no space before the
fraction. The fraction is set small, in a smaller size or in the font's own
fraction figures, its numerator high over a slash; some fonts draw `½` whole
and spell it `1/2` in the text. The text layer gives the digits alone, and
read as one number `22 3/8"` is `223/8"`. A fraction set small after a whole
number is read apart from it, `22 3/8"`, and a numerator raised over a slash
is a fraction's, not a power: `78 5/16"`, not `78⁵/16"`. Old-style figures
are as short, but stand on the baseline, and stay as they are.

**Display type beside text.** A cover title or a margin title set at two and
a half or more times the size of the text beside it shares a line with it
only where the two stand on one baseline to within the smaller text's own
jitter, and no wider than a gutter measured in that text's size. Measured
in the title's size, a 28pt title took the line of a 10pt description that
sat 5pt from its baseline, and cut the description in three around it. A
title stacked in a margin, read line by line with the column beside it, is
put back together as one block, its number and its words in whatever
weight: `III. Regulatory cholesterol`. Display type is never a list marker.

**Drop caps.** A paragraph's first letter set large, its top at the top of
the first line and its foot on the second's, third's or fourth's baseline,
is read at the head of the first line and joined to its word: `This report
defines`, not `T` on its own, nor `T of those stories` two lines down.

**Registered and trademark signs.** `®`, `™`, `©`, `℠` and `℗` belong to
the word before them at any size: a 28pt title's `®` at 8.4pt, or a 3.5pt
`®` after a 10pt `ProCat`, which came out `ProCat .` with the sign lost.

**Text the producer states.** Where a PDF wraps what it draws in marked
content with `/ActualText`, that text stands for the glyphs inside: a browser
states `青` over a glyph its font maps to the radical `⻘`, and some producers
state the digits of a page number whose glyphs map to nothing. It is taken
when it says more than the glyphs do. Text that only restates the drawn
letters in another case (a heading set in capitals over lower-case text), or
that holds nothing but spacing and punctuation (a tab, a soft hyphen, a dot
leader), leaves the glyphs as drawn.

**Characters as printed.** Text keeps the characters the PDF's text layer
gives, as the other formats keep theirs: `µm`, `½`, `™`, `（２）`, `m³/h`.
Ligatures arrive as single codepoints, and searching for `profile` would find
one occurrence in six, so they are read as their letters, as are the other
presentation forms and a Kangxi radical a font names for an ideograph. That
can change string length, so the engine keeps a bijective map between raw
glyph offsets and text offsets. See [normalization](../formats/text.md#normalization).

Measured Unicode resolution: **99.97%** on a 200-document Latin corpus,
**99.73%** on CJK, with **zero** replacement characters on Japanese and
Chinese government documents.

## When PDF is hard

Two failure modes, both detected rather than guessed:

- **The text is pixels.** A scanned page has no glyphs to analyze; an OCR
  engine reads it, the built-in one also when the sheet was fed upside down
  or sideways (see [OCR](../cli/convert.md#ocr)).
- **The text is garbage.** Broken CID fonts yield glyphs whose Unicode is
  unknown or wrong — the layout can be perfect and the text worthless.

`fdoc triage` reports both. See [the router](../design/router.md).

## Encrypted and malformed files

`extract_file` returns `ExtractError::Parse` for a PDF that cannot be opened.
A panic on real input is treated as a release blocker rather than a bug
report, so malformed files fail as errors you can catch.

A PDF is read within [limits](../reference/limits.md): its pages to the
10,000th (those past it listed unread, `PastPageLimit`), what they draw
within the document's memory and work budget, its page tree at most 256
deep and 250,000 pages, and a page rendered for a crop with at most 64
million pixels and 32,767 a side. Past the budget or the tree's bounds,
`extract_bytes` returns `ExtractError::TooLarge`, which says which limit.

## Performance

~8 ms per document for the deterministic tier, ~2.4 ms per page for parsing
alone, ~420 pages/s. There is no model to load, so the first document costs
what the thousandth does.

## Annotations and embedded files

A reviewer's sticky note, the comment on a highlight, a free-text box typed
onto a form: none is drawn by the page's content, and each is text to a
person reading the page in a viewer. Text (sticky note), `FreeText`,
`Highlight`, `Underline`, `StrikeOut` and `Squiggly` annotations with
something written in them (`/Contents`) are read, each a `doco:Paragraph`
after its page's own text, with the annotation's rectangle as its `bbox`,
`evidence` `annotation`, and its author (`/T`) opening its text as a
transcript turn's speaker does: `Sam Example: Check this total`. An
annotation is read once however many times or pages list it, a note keeps
at most 65,536 characters, at most 20,000 notes are read, and each is
counted, with its text, against the memory one document may take; a
document past it fails as too large. What a
highlight covers is the page's text already, and is not repeated. A note is
not counted as the page's text when deciding whether a page was read, so a
scan with a sticky note on it is still reported unread. Link annotations
are read as the `links` of the text they cover, and form widgets by
[`fdoc forms`](../cli/forms.md).

**Embedded files** — the catalog's `/EmbeddedFiles` and the files
`FileAttachment` annotations carry — are a document's attachments, as an
e-mail's are: listed in DoCO's `doc:attachments` (filename, content type,
size, SHA-256), each once by its bytes, and saved by `--attachments DIR`
under `DIR/<input name>/`; without it, a note on stderr names what was left
out. Each is decoded within bounds: at most 1,000 files, 64 MiB each and
256 MiB in all, inflated here rather than by the parser, whose decoders take
no bound. A file is decoded once however many entries name it, and what
decoding inflates is counted whether the file is kept or refused, at most
512 MiB for the document, so entries naming streams that inflate past
their bound cannot make a small file costly. A file encoded otherwise than
with Flate (or not encoded), one past a bound, or one that does not inflate
is named in a warning, once, and left out. In a library, `fluree_doc_pdf::annot::annotations` and
`embedded_files` return both.

