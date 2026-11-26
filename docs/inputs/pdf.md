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
  [Links](../formats/markdown.md#links). A link over a note's raised number
  covers that number (`analysis[⁵](#page=7)`), not the next `5` on the line.
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

**Capitals mapped to lowercase.** Some producers write a font's character
map with some of its capitals folded to lowercase: the code that draws `F`
reads `f`, and a reader that trusts the map gets `board of Governors of
the Federal reserve System`, or a table's row label `foreign currency
translation adjustments`. Where the font's encoding is a standard one
(WinAnsi, MacRoman or Standard, with its differences) and names, for a
code, the same letter as the map in the other case, the encoding's letter is
read: it names the glyph drawn, and the width the PDF gives the code is that
glyph's. A map that reads a code as anything else, a small capital, a
ligature or a symbol font's glyph, is kept.

**Accents set over their letters.** Many typesetters draw `é` as an `e` and a
spacing `´` placed over it, and TeX draws every accented letter and every
hat, tilde and bar in its formulas that way. Read in order that is two
characters, `Balague´`, `K¨ahler`, `A˚`, where the page shows one. A spacing
accent (`´` `` ` `` `ˆ` `˜` `¨` `¯` `˘` `˙` `˚` `˝` `ˇ`, and `¸` `˛` under the
letter) whose ink is centred on a letter's and touches it or nearly does is
read as part of that letter: `Balagué`, `Kähler`, `Å`, and `K̂` or `x̄`,
letter and combining mark, where Unicode has no single character. An
accent over a dotless `ı` reads as an accented `i`. The same character set
beside a letter is left as printed: an acute standing in for an apostrophe
(`Spearman´s`), a prime after a figure, inch marks after a dimension.

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
however tightly the next word follows. One drawn in sequence but set back
onto the ink of the letter before it, with the next letter where that
letter's pen stopped, leaves no space on the page and is ignored too: a
producer sets one after each `ﬁ` ligature (`Scientifi c`, `califi cado`).

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
broke, and reads as the word. A lowercase compound broken at its own hyphen
looks the same as a word broken inside, and the document tells them apart:
where it sets the compound whole on a line elsewhere, and never closed up,
the hyphen stays (`treatment-` over `emergent` is `treatment-emergent`
where the text has `treatment-emergent AEs`). A compound the document sets
only across a line break reads as one word. Some producers map the hyphens they draw to
U+00AD SOFT HYPHEN; drawn, it is read as the hyphen it prints, and one that
draws nothing is dropped.

**Dashes and addresses at a line's end.** A line ending in an em or en dash
set closed against the word before it joins the next with no space, as the
text's own hyphen does: `restore—` over `D2D` is `restore—D2D`, `10–` over
`20` is `10–20`. A dash with a space before it was set apart and keeps its
space. A web address broken at a slash (one with `://` or opening `www.`)
runs on into the next line whole; a slash ending any other word may have
had a space after it on the line the break took, and keeps it.

A table cell needs one more piece of evidence. A narrow column breaks a
compound at its own hyphen (`long-` over `term`) far more often than prose
does, and the letters alone cannot tell the two apart. So a cell's word
broken that way reads whole only where the document prints it whole on a
line somewhere else: `Fluo-` over `rescent` is `Fluorescent` in a sheet whose
title says `Fluorescent`. A word the document prints hyphenated, or nowhere
else, keeps its hyphen. A note's number raised after the word is no part of
it: `consoli-` over `dated²` is `consolidated²` where the document prints
`consolidated`.

**A face's weight.** Each glyph takes its weight from its font: from a
weight its name states (`Arial-BoldMT`, `HelveticaNeueLTStd-Bd`, `Sohne-Halbfett`,
`NimbusSansL-Regu`), else from the `/FontWeight` its descriptor states, else
from the font program. The name goes first because it is the source least
often wrong: a subset whose program says bold for a regular face, or a
descriptor that says 400 for a black one, still carries its face's name. A
medium face is weighed by its ink against the body face, or, setting only
a few letters, by the stem width its descriptor states, since some families
set their medium as the bold and others barely darker than regular. So is a
face nothing states a weight for, where the body text's face states none
either. In a
document set in a light face, every weight is read against it: a medium
over a light body reads as bold.

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
paragraph. An item opened by a letter or a number (`a. For 120/277/347V
connect the black fixture lead to the` over `voltage supply …`) hangs its
wrapped lines the same way where its sentence plainly goes on: the next
line opens in lower case, or the item ran out of room. A line that opens
with a dash, or follows an item ending on a colon, starts a list of its own
under the item. A sentence an item broke onto a new line by hand, set at
its hanging indent at the item's size and leading, goes on with the item
when the list goes on after it: only the paragraph after a list's last
item is the page's.

**Bold or large, but no heading.** Size and weight do not make a heading
of a field (`CRI: 90+` at the body's size, or a label with its value
beside it), of a value
set under its bold label at the label's size (`K25W951` under `PRODUCT
NUMBER`) or of the label over it, of a form's label waiting for its value (`Location:`, `Model
#:`), of a form's column keys and answer heads (`(a) (b)`, `Yes No`), of a
line's caption between its letter and number (`b Refunded 37b`), of a
sentence that refers elsewhere (`…, see your tax return instructions.`), of
a chart's axis title set up its side or under its scale's values, of
a table's column head or stub over its figures where the table is not read
as one, of
a signature block's role and party (`INVESTOR:` over the party's name over
`By:`) or a signature page's label, or of a letterhead's lines over the
sender's address and numbers. No heading stops on `and`, `of`, `the` or
`not`: such a line runs on into the next.

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
a guideline's subsection), so there the heading pass decides. A
mathematical operator set apart beside a formula's bar (`∂`, `∑`) is the
formula's text. It is read as a marker only at the margin of what it would
mark, as a number is.

**Drawn bullets.** Some producers draw a list's bullets as small filled dots
or squares and leave no bullet in the text. A filled mark under a checkbox's
size, set just left of a line and level with it, opens a list item, as long
as another such mark at the same place opens another line. A list draws a
bullet for each item, in a column. A seal's ring of dots or a chart's
scatter is level with a line of text only by chance. A drawing's dots also
crowd one another, where a list's bullets stand a line apart. So a mark with
another dot closer than half an em, in the size of the line beside it, is a
drawing's and opens no item. Examples are a map's towns beside the title of
its district, and the modules of a QR code beside its caption. A list of one item
draws one bullet, and it marks its line when it is drawn like the page's
other bullets: their size, as far from its text as theirs.
The item's marker is `•`. Each item starts where its bullet is, however
closely the items are spaced.

**Numbers set apart.** A list's number may stand apart from its item, left
of the item's first line: a form's `2` beside `Transaction reported to you on
Form 1099-B?`, a datasheet's `(1)` beside its note, `[37]` beside a
reference. The item's text keeps the number as printed (`2 Transaction
reported…`, `(1) Stresses beyond…`). A bullet's glyph, by contrast, is only
the item's mark and is not kept. A number counts as part of a list only when
all of these hold:
- the list counts: another item's number is one more or one less, and a
  letter or roman list runs from its first (`a`, `i`) to this one;
- it stands level with the item's first line, within three tenths of the
  line's size;
- it is set at seven tenths of the item's size or more, and is not raised
  as a note's mark is.

Anything else is left out of the item's text, as before. That covers a year
over a table's rows, a fraction, a dimension, a chart's tick labels set
beside a column, and a word spelled down the margin a letter a line.

**Dashes and arrows.** A line opening with a dash, a minus sign or an
arrow and a space (`—`, `–`, `-`, `−`, `→`, `>`) is a list item when
another line nearby opens with the same mark at the same place. A `>`
opens an item only before a capital or a figure: a quoted message's lines
open with one too, going on with their sentences in lower case. The lines between two such lines must hang
under the item's text or be a word or two (`or`). The mark is the item's
marker, as a bullet is, and wrapped lines hang under the item's text. A lone
line so opening, among lines of prose at the same place, is a range, a minus
or a dialogue's dash, and stays as it reads.

A glyph that says nothing as text marks items the same way: a symbol the
font maps to a control code or to the private-use area, or an arrow, a
shape or a dingbat. A sidebar whose `▶` bullets the font maps to the
control code 0x17 reads as a list, and as such a mark is no range or
minus, its items may wrap over any number of lines. No control character
other than a tab or a line break is written into the output, from any
source.

**Nested lists.** An item whose marker stands under the text of the item
before it is in a list nested in that item, one level deeper (`depth`). A
bulleted `UL Listed to control:` has its dashed loads set under its text,
and a regulation's `(1) … from—` has its `(a)` and `(b)` set under its own.
An item whose marker stands back out returns to the level its marker stands
at. The marker must stand at least an em right of the marker above, from
an em before that item's text to two and a half ems past it. A marker
further out belongs to another column. Only an item that follows the one
before it down the page nests. A run broken by anything else, carried to
the next column or onto the next page, starts again at the top. A wrapped
line is its item's own, and a paragraph set with a hanging indent has no
marker, so neither nests. Neither does a line read as an item whose number
stands beside a later line: it carries on the item before.

A marker is placed where it is drawn. One list can set its first bullet in
the line and draw the rest apart from it, so one item's text starts at its
bullet and the next item's a bullet's width in. They are compared by their
bullets, which stand level, so the items are siblings. A box to tick marks
a choice rather than a list's lead-in: what is set beside or under a form's
`Yes` and `No` boxes stays level with them.

**Numbered lists.** A feature or safety list often sets each item's number
in the item's own text, `2. PostureFit SL®`, with a line or two of
description under it. Such a list is read as a list, each item's title and
description one item, when:

- the numbers count off from 1 at one left edge, at least three of them;
- each has only a few short lines under it, none opening with a number of
  its own;
- the count stops with the list;
- the list sits inside a section, below a heading other than the document's
  title.

A list carried on in the next column is the same list. The following stay
headings, because they are the document's sections:

- numbering that carries on to a later page;
- numbering that has clauses numbered `1.1` under its numbers;
- numbers set straight under the title (`1. SCOPE OF SERVICES`, `2. TERM`).

So does a numbered line that the PDF's outline names. In Markdown such an item is written with
its own number (`3. FAQ`), not after a bullet.

**Items one per line.** Limitations and table notes often number their
items with nothing between them but the line spacing, each item's wrapped
lines hanging under its text. A line opening with `2.` set where the line
opening with `1.` is starts the next item, and the lines under it stay with
it; a run of such items is an ordered list, even across a slightly wider
space between two of them. A list of entries set one per line, a standard
or a code on each, reads one entry per paragraph when every line stops well
short of the column's width and none goes on in lower case. Bold lines
stacked so, each opening with a capital that would have fitted on the line
before, are headings one per line: `Materials and methods` over `Animals`.
And a
paragraph ended on a short line, followed at the same spacing by a line
opening with a capital, is two paragraphs: the short line stops more than
the next line's first word and two ems short of the lines around it.

**Fields and addresses one per line.** A run of lines that each open with a
field's label, a few words from a capital ending on a colon with the value
after it (`Klantnummer: 6669263`, `Ust-IdNr.: DE123456789`), reads one field
per paragraph; a value wrapped onto the next line stays with its field, and
a field is never read as a heading. A label is no sentence carried onto the
line (`subgrouped by age:` opens in lower case), nor a reference's volume or
a list of abbreviations run on from the line above, and no field starts
after a line ending on a comma, a semicolon or a word its phrase goes on
from. A block of short lines stacked by hand, an address or a contact block,
keeps its line breaks in one paragraph: five words or fewer to a line, all
at one left edge and at the body's size, not all bold, and each line after
the first opening an entry of its own (a capital, a figure, an e-mail or web
address, or a telephone number, `fon (555) 23 78 -0`). A line ending on a
hyphen, a comma, a dash set closed or a web address broken at its slash runs
on into the next, and so does prose set in a narrow measure, which goes on
in lower case. A heading's lines, broken by hand, still read as one title.
Wrapped text, even with a designer's rag, never does. The other way round,
a sentence that runs on stays one paragraph across a slightly wider line
spacing, as footnotes set at a generous leading have, or across a change to
bold capitals within the paragraph: the line is full, ends without a full
stop, and the next goes on in lower case or in the same capitals.

**Lettered items.** Steps and sub-questions lettered in lower case, `a.`,
`b)`, `(c)`, form a list in two cases:

- the lines count off from `a.` at one left edge, each item's wrapped lines
  hanging under it;
- a block opening with `a.` has a block opening with `b.` set where it is
  under it, each a paragraph long or a step wrapped over lines.

Letters count apart from numbers. So questions numbered `2.` and `3.`, each
with lettered sub-questions under it, read as two numbered items. The lines
set left of the last lettered item, where the numbered text goes on
(`2. When changing…` under `f. The depth of focus…`), are not part of that
item. A lone `a.` opens a paragraph. Lines that happen to start `(d)` and
`(e)`, a caption's panels wrapped one to a line, count off from nothing. A
capital, `A.`, opens an appendix or a section as often as an item, and is
read as neither here. In Markdown a lettered item is written after a bullet,
`- a. …`, since Markdown numbers only with digits.

**Footnotes.** A block of footnotes, each opening with its number raised at
the margin and set at the text's leading, reads one note to a paragraph,
each opening with its number. A line whose raised number counts on from the
note above it (`7 1 MB/s = …` then `8 Portion of buffer…`) starts the next
note. A raised number the text layer set apart from its line is the note's,
not a list's marker: it stays in front of its text, where the body's note
marks can find it. A line opening with its note's reference marks, a run of
up to four asterisks, daggers or section signs (`*`, `**`, `†`), starts a
note of its own, even under a full line of the note before: `*** Zero waste
…` under the `**` note's last line.

**Note marks.** A note's mark is drawn as a smaller glyph raised above the
baseline, right against the word it marks, and the text layer joins the two:
`Acrylonitrilea`, `Revenue1`, `Flarec (50100410)`. Read into the word, the
mark changes it. A note's number keeps its place raised, written in Unicode
superscript digits as a power is: `Revenue¹`, not `Revenue1`. A note's
letter or symbol is read apart from the word instead, since Unicode has no
full raised alphabet to write it in: `Acrylonitrile a`, `Total *`.

A raised number is a note's after a word too long to be a unit, after a
quantity (`26TB¹`, not `26TB1`), or after a bracket that holds words
(`(49 N)⁵`, not `(49 N)5`). So is one after a compound or a code joined by
hyphens (`3-way¹`, `MACL-153M-XX²,³`), and after a bracket whose slash parts
names rather than terms (`(512e/4Kn)¹,²`, `(mm/in, max)⁴`). So is a number
after a word and the comma or stop closing it (`MTBF,⁴`, `countries.⁵`),
since no power is raised past a word's punctuation, and a number closing on
a bracket of its own, as notes are numbered in much of Europe
(`dilution series¹⁾`). A comma between two notes' numbers has no raised
form and stays as it is.
A raised number set before a word, after a space or at a line's head, is a
note's number before its note or the name it is on. It is read apart from
the word and written plain, as a list's number is (`4 M17 series`, not
`4M17 series`): at a note's head it is the note's own number. Raised text
that belongs to its word stays in it: a unit's power, kept raised (`cm³`,
`m/s²`, and the Spanish square foot `PIES²`), an ordinal's ending (`1st`)
and a formula's index or power (`ρwLv²`, `Vol(∂Ω)ⁿ`, `(a+b)²`).

A font may draw its own superior figures instead: set at the text's size and
on its baseline, but drawn small and high. They are read as raised marks by
their ink, which stands clear of the baseline and short of a figure's height,
so `Lengths` and its superior `7` read `Lengths⁷`, not `Lengths7`.

After a number, the same marks matter more, because the text layer turns
them into another number. A note's letters stand apart from the value:
`110ᵉ` reads `110 e`, and `610ᵉ,ᶠ` reads `610 e,f`. Raised digits and signs
after a number keep their place as Unicode superscripts, whether a power
or a note's number: `kg/10⁶ dscm`, not `kg/106 dscm`; `4.0x10⁻³`, not
`4.0x10-3`; `2024³`, not `20243`. An ordinal's ending stays with the number
it agrees with (`22nd`, `118th`, `21e`).

Some producers write a space between a formula's letters and draw the
subscript into it afterwards: `As`, a space and `O`, then the `2` of As₂O₃
set back where the space stands. A space whose room a script fills — a
glyph after a letter, smaller than it and off its baseline, starting inside
the space's advance — is no word space, and the script joins its word:
`As2O3`, `HNO3`, `NH4OH`, not `As 2O3`, `HNO 3`, `NH 4OH`. The same holds for
a symbol's subscript (`τ1`, `χ2`).

A degree sign set as a raised `o` or `0` reads as `°` wherever it cannot be
an ordinal's ending or a power: before a temperature's unit (`25°C`, not
`25oC`), after three figures or more (`~ 145°`, not `~ 145 o`), after a
decimal, after a signed number, or after an approximation's sign, where no
ordinal stands (`~ 95°`). After a bracket's opening only a raised letter `o`
does (`Gloss (45°)`): the ordinal sign `º` is how Spanish and Portuguese set
a bracketed ordinal, and `(2º)` stays one. `1º` and `2º` stay ordinals and
`10⁰` a power.

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

**Currency signs set apart.** A statement or an invoice may set a figure's
currency sign in a column of its own, a little left of the figure:
`Totaalbedrag  €  22,50`, a column of `$` beside a statement's amounts, a
form's `£` before its box. Such a sign alone on its line is the figure's
unit, never a list's bullet: it stays in the text, and the figure beside it
is no list item.

**Side labels.** A form sets labels in its margin a step larger than the text
beside them, such as Form 1040's `Sign` / `Here` at 12pt beside a 6.9pt
declaration. The gutter after such a label is measured in the text's size
when all of these hold:
- the label is a word or three of letters;
- it is set a fifth or more larger than the text;
- the text beside it goes on in a column of its own, its next line starting
  where it starts.

Measured in the label's size, `Sign` was read into the declaration and
`Here` stood alone. A label's lines read apart in two cells of the page
join again as one label (`Sign Here`, `Third Party Designee`, `Acceptable
Means of Compliance`). They must be one-line blocks at one size and one left
edge, one under the other, set over the page's text, with smaller text
beside them. These stay as they were:
- a clause's number set apart (`12.3`), read as a marker;
- a speaker's name before the speech, its next line under the name;
- a field's label before its value (`Title: Member`).
Nor is a number set beside another number, such as a chart's tick labels
side by side (`2.4−` beside `2.6−`): a list's number opens an item that
says more than a number. Taken for markers, the years under a chart's bars
vanished from the text.

**A callout beside a line.** Text set beside a paragraph's line at the same
size, on a baseline a little off the line's, is a line of its own: a line is
read along its row, and where the next glyph read would put the pen back more
than an em behind the line's end, a new line starts. A flyer's bold callout,
`GreenCool Technology`, 0.3 of its size higher than the body line to its
left, was read first and the body line after it, unspaced
(`GreenCool TechnologyThe refrigerated models…`), and the paragraph was cut
there.

**Side heads.** Some data sheets set each section's heading in a narrow strip
at the left, level with the first line of its text in the wide column beside
it. Such a strip is read with that column, each head just before the text it
is level with, so each section holds its own text. The strip qualifies when:

- it is narrower than the column beside it;
- most of its blocks are short lines, at least two of them;
- each of those starts level with the top of a block or a table in that
  column.

A block of the strip level with nothing, such as a tag in the corner of the
page, is read where its height puts it.

**Regions.** Pages rarely run their columns from top to foot. A title band
sits over two columns, two sections share one pair of columns, a box of
text in two columns sits under full-width text, a picture's caption spans
two of four columns. So a page is first parted into regions, at what runs
across its text: a title across the columns, a rule or a table across them,
or a stretch of white space wider than a paragraph's. Each region is read
column by column before the next. A cut never runs through a paragraph: a
caption, rule or table that spans only some of the columns leaves the
paragraphs beside it whole, and a link's underline is part of its line. Two
charts or cards side by side, each opening with a title on one row, are each
read whole before the next. A box set in two columns of running text under
full-width text is read one column after the other, even where the page as a
whole has no gutter there. A field list keeps each value with its label, and
a side tab's turned labels at the page's edge are not weighed.

**Blocks side by side.** Two blocks of text set side by side within one
column's stretch are each read whole, the left first, or the right where it
starts a line or more higher: an invoice's seller beside its customer, an
article's details beside its abstract, a paragraph beside a chart's title
and notes. A gutter of a line's height or more parts them, where neither
runs on as prose does, when either holds:
- Each is set on baselines of its own: most of the shorter one's lines
  stand level with none across.
- The left one goes on alone two lines or more past the right one's last,
  and its lines are not labels (ending in a colon, or bold). A column of
  labels beside their values stays as rows, each value read after its
  label: a value may wrap past its label, but labels do not run on alone.
Both sides must hold text, half their lines of two words or more, so a
chart's ticks and a column of figures are not blocks.

**Boxes side by side.** Text set in drawn boxes, outlined or filled, is read
box by box. Where every line of a stretch stands in a box, the boxes are read
in flow order: top to bottom, and left to right across a row of boxes level
with each other, so a flowchart's branch, set lower at a box's side, reads
after the box it leaves. Elsewhere, a row of two or more boxes side by side,
each holding text and one of them two lines or more, is read box by box,
left to right: two boxes of a diagram, two charts' panels with their titles
in bands. A box holds the boxes inside it. Text over the row, or level with
it at its left, such as a product's name beside a row of its ratings, is
read first. A table's cells drawn one box each touch across the row or down
the column, and are left as rows.

**Tables and boxes in the reading order.** A table is read where it stands
in its column, under the heading it sits under: one above it in its column,
or one beside it at the left, either just above its top or level with its
first row, as a side head in a column of its own stands. A label beside the
first row is no head when other labels stand down the table's side below it.
Those are the rows' labels. Two tables side by side under one heading are
read left then right, each followed by the note under it. A line stops at a
rule drawn up between two of its words, such as the side of a box between a
letterhead and the form box beside it. The rule must run the line's height,
stand in a word space at least, cross no text along its length, and stand
beside two rows of text or more on each side. So a chart's line drawn behind
its labels parts nothing, and neither does the border between a single row's
cells: a code and what it names stay one line.

**Dot leaders.** A table's label often runs on in dot leaders up to the next
column, a word space or less short of its text (`Kolkata, India ........
Manufacturing`). The leaders end the label's cell: the text after them starts
the next one, and no cell keeps its leaders. A column of labels whose leaders
run up to the table is the table's, however far its words stand from the
figures, and a line of leaders left between two cells is not the page's
text. The dots must stand close together, so a table's empty values (`..`)
in neighbouring columns are no leaders. A contents entry, whose leaders run
to a page number alone, is left as a line of text: a contents list is not a
table.

**Columns run together.** Many producers write the space after a word as a
glyph that puts no ink down. Where the next glyph on the line stands across a
gutter, the gap is measured from the last glyph that put ink down, so a
column's line and a callout, or the next column's line, are read apart. A
justified line's stretched space stays a space: the lines of its paragraph
run across it. A number or a label set off from its text by a tab, `3.1 ⇥
Results` or `Note 3. ⇥ Business Segment`, stays with its text. A label
that heads a block of its own in the margin, in another size from the text
beside it (`Issued 07-2025` over `Supersedes 05-2025` beside a note), is
read apart from it.

**Drop caps.** A paragraph's first letter set large, its top at the top of
the first line and its foot on the second's, third's or fourth's baseline,
is read at the head of the first line and joined to its word: `This report
defines`, not `T` on its own, nor `T of those stories` two lines down.

**Registered and trademark signs.** `®`, `™`, `©`, `℠` and `℗` belong to
the word before them at any size: a 28pt title's `®` at 8.4pt, or a 3.5pt
`®` after a 10pt `ProCat`, which came out `ProCat .` with the sign lost.
Producers often set a space glyph after the sign, at the sign's size, at
the next word's, or on the baseline of a smaller line beside the title. A
space draws nothing, so it never decides which line a sign is on, and it
goes with the sign: `Comfortboard® 80`, not `Comfortboard 80` with a `®`
heading the header line beside it. A sign alone on a line is never taken
for a list's bullet.

The line goes on past the sign from the word the sign marks. A 32pt
title's `®` raised 19pt, to the cap height, is far above the baseline its
title's next word is set on, and the title broke there: `PROCAT®` and
`PROFESSIONAL`, two lines on one baseline. A sign is measured by its own
size, not by the space set after it: a `™` with a space at the 17pt title's
size behind it is the title's sign, `Emerald® Symmetry™`, not a run of text
as large as the title. The same holds in running text: `Truebeam® 2.0
linear accelerator` is one line.

**Display titles over several lines.** A title set in display type and
broken over several lines, each line centred or right-aligned, is one
block: `PROCAT® PROFESSIONAL INSULATION SYSTEM`, not four headings. Such
lines start where their length puts them, so their left edges differ by
design. Display type means larger than the body text of the page's prose,
measured without the page's tables. Lines set at body size that share a
centre, an address ended by hand, stay apart, and so does a line opening
with a list's number.

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

## Charts and diagrams

A chart's text extracts perfectly and still means the wrong thing read in
page order: a donut's region names and percentages interleave by position,
and half the pairings a reader would make are wrong. So the text of a drawn
chart or diagram comes out as `doco:Figure` elements, each carrying its
figure's identifier, with evidence `fills`. Nothing is moved or dropped; the
elements say these fragments belong together, here.

**What a drawing is.** Drawings are found from the shapes a page draws, not
from its text:

- A filled shape is measured by what can be seen of it. A polar chart that
  strokes whole circles and clips them to a quarter, or artwork drawn past
  the page edge, counts only where it shows.
- A shape with text set on it to be read is that text's background, not part
  of a drawing. That covers a panel behind a paragraph, a band behind a
  specification row, a callout box, a side tab, a form's boxes and a table's
  shaded cells, along with the strips along a panel's edges. A table's cells
  go together: a cell holding a number, or nothing, that abuts one holding
  words in its row or its column is a cell too, and shading set as cells in
  rows and columns is a table's, however it is grouped. A plot's background,
  holding the bars and scattered labels, is part of its chart. A diagram's
  boxes hold labels set well under the page's text, and are part of the
  diagram.
- A scatter plot's points line up in rows and columns as a table's shaded
  cells do. They are not read as a table's shading when all of these hold:
  - they are marks smaller than a line of the page's text, and most of
    the group's shapes;
  - they are drawn as a few symbols;
  - they spread both ways and fill few of the places their rows and
    columns cross;
  - they are plotted against an axis: three numbers or more beside them,
    set in a column on one right edge or in a row on one baseline.

  A calibration's points, at a few levels across, each at nearly the same
  height, are such a scatter. A matrix of dots, a row of text apart, a
  column of drawn bullets, and a seal's ring of dots are not.
- A plot drawn in lines alone fills nothing: a scope's graticule under its
  traces, or a log plot's grid under its curves. It is a drawing when its
  lines span it both ways, four or more each way, and stand evenly spaced
  each way, with text in under two of every five of the cells they make.
  A line broken off for a label or a trace leaves its neighbours a whole
  number of steps apart, and the grid still counts. A ruled table's text
  fills most of its cells, and its rows and columns differ in size.
- A line chart drawn in strokes alone fills nothing: its series are lines
  of many straight segments, most of them diagonal (a series sampled
  densely steps under half a point per value, and leans against its own
  step), and its axes are a rule under the plot with ticks at an even
  pitch, or a column of ticks beside a scale. A series bound by such an
  axis is a plot when it runs one way across the page, as a series plotted
  against its axis does; its drawing is the box its axes bound, up to the
  top of the axes standing at its sides, so a title set over the plot is
  measured from the plot's top and not from the series' highest point. A
  table's rules, a statement's underlines and a form's boxes
  run square and draw no series. An outline, a symbol or a drawing set
  behind a table turns back on itself, and a line with no axis by it is no
  plot. The text of such a plot is left in the measure of the body's size,
  as the page's headings are read against it.
- A bar chart's bars are fills of one width, three or more at an even pitch,
  each standing on one base or hanging from it, at lengths that differ.
  Pieces that abut, each ending where the next starts, are one stacked bar;
  pieces a gap apart are bars of their own, so charts set one over another
  are read apart. Bars are a chart when an axis binds them: the base drawn
  as a rule across them, a column of ticks beside them at an even pitch, or
  a scale beside them whose numbers step evenly. Bars set across the page
  are read the same way. A table's shading is no chart: its rows run to one
  length and hold their text, its figures step unevenly, and its columns of
  cells end, piece after piece, at the same rows across the table, where a
  stacked bar's few pieces end at their values.
- A box diagram, a flowchart's steps or a system's blocks, is three boxes
  or more, each holding a label, standing apart and joined by two
  connectors or more: a line or an arrow crossing the gap between two boxes
  where they face each other. A connector may also be curved, outlined as a
  shape, stopped short by an arrowhead drawn apart, or run across between
  boxes set apart both ways. Then it meets exactly two boxes and holds no
  text. Between boxes side by side or stacked, it is thin across the gap and
  runs through the middle of the facing sides. Between boxes set apart both
  ways, it stays within the pair's reach. So a table's cell dividers and
  stripes, and a chart's shapes running on past its labels, join nothing.
  Its labels are the ones set in its boxes, with
  its banner and side tabs standing against it and a caption over or under
  it; a panel of text beside it stays the page's. A table is no diagram: a
  box holds a label, not a row of figures; a band as wide as the page is a
  table's shaded row; a table's group underline runs along its header
  cells' edge rather than across a gap; and no box lies within a table
  already found. A table's caption under a diagram stays its table's.
- A form's fields are no drawing's shapes: an empty box a line or two high,
  under or after its label's colon (`Job Name:`, set at the text's size, not
  a title's), and the boxes in line with it at its height. The box that
  holds them is the form's background. So is a band, at least three times
  as wide as it is tall, holding marks kept to one end, under half its
  width, with words along the rest of it: a footer's certification logos
  beside `APPROVALS:`. A submittal's logo set over its form is a logo, and
  the form's labels are the page's.
- A word drawn as shapes, a logo's lettering, is a drawing of its own:
  three letters or more on one baseline at one height, side by side, clear
  of every other shape by more than their height. A submittal's wordmark
  under its wiring diagrams no longer joins them by reach, and text set
  beside the wordmark on its row (`SPECIFICATION SUBMITTAL …`) is no label
  of the diagrams. Its letters are no boxes to tick either, and neither are
  a table row's shaded cells set side by side, nor a drawing's pieces: a
  small square of a logo's illustration, lying in a shape that holds other
  pieces or overlapping one part way, set beside an address.
- A background as large as the page joins nothing. Drawings set close
  together are found as separate drawings, rather than lost as one shape too
  large to be a drawing.

**What a figure's text is.** A figure holds the text set inside its drawing,
and the labels around it:

- text beside, above or below the drawing and near it, or set smaller than
  the page's prose and near it or near another label;
- never further from the drawing than the drawing's own size.

A line at the prose's size is no title in two cases:

- It stands in the page's head band, the top 8% of the page, more than two
  lines from the drawing. That is a running head, even one that names the
  appendix the plots are in. A badge's small words there are labels as
  anywhere.
- It says three words or more and its sentence runs on into the page's
  prose set under it, starting in lower case. That is a paragraph's line,
  even when it was parted at a wide justified space. A legend's entry of a
  word or two doesn't run on.

A list item, a sentence of more than fifteen words, and a block too wide to be
a label stay the page's, however close they are set. Text at the prose's
size is a label only as wide as the drawing, give or take half again: a
chart's title, legend or axis name. Two lines of prose beside a small QR
code or an icon run well past it, and stay the page's. So does a line that runs
on, at the same size and left edge, from text the figure did not take, such
as the last of a column of notes or the first bullets of a list. So does
text in a column of its own beside a drawing, level with it: a heading or a
note set flush with a paragraph of the page's in that column (`FCC` and its
caution beside an install guide's drawing), a run of steps wrapped from
line to line (`… the black fixture lead to the` over `voltage supply …`),
and the line introducing them on its colon. So is a line that introduces a
list on its colon, the list's items set just under it. A label is terse, and stands
among the drawing's. An address is no label either. A line that closes on a
phone number, a web address or an e-mail address says where to reach the
page's maker, not what the drawing shows. The lines it runs on from go with
it, such as the company and street over `1-800-…` and `www.…` set under a
logo. The rectangle
around a drawing and its labels is not part of the figure. A paragraph or
heading inside it that was not taken as a label keeps its type. Nor is a
paragraph of the page's prose ever a figure's, taken or not: set at the
body's size, over two lines or more, saying more than fifteen words, and no
caption or note. Drawings grouped across a column, or a page of product
panels, can enclose the text between them; that text, and a heading set
flush over it, stay in the reading flow. A box diagram's steps are its own.

**A frame drawn round a drawing bounds its figure.** A wiring diagram set in
a box, with its labels in the box's margins (`Line / Hot` left of the
switches, `Neutral` under them), takes every label in the box, whatever
their distance from the shapes. Where one box holds several diagrams, each
label goes to the nearest. Outside the box it takes only its caption: a
`Figure 17. …` set over or under the box, a `Note:` or `Source:` under it,
and the lines that run on from them. A note in the column beside the box and
a title over it that is no caption are the page's. A box is a frame
when its drawings span most of it each way, something sits in its margins,
and all it holds is labels: fifteen words or fewer each, and no display
type. A slide's background holds its title, and a plot's own background is
filled by its bars, with the axes' names and the legend outside it; neither
is a frame.

**Between two drawings.** A title set under one drawing and over another,
across both, goes to the one it stands nearer. Distance is measured to each
drawing and the labels only it took. If both are equally near, the title
goes to the lower, since a title stands over what it names. A diagram's
title set under the diagram above therefore heads the next. Where the lower
drawing is framed, the title is the page's, as any title over a frame that
is no caption is.

This applies only to display titles: text set larger than the prose, in
words, and no caption. A panel's axis name and ticks stay with the panel
above, however close the next panel's labels stand.

**Captions.** A figure's caption, `Figure 3.`, `Fig. 2` or `Chart 17` and
its text, goes to the figure it names. A document sets its captions on one
side of its figures, above or below; the captions that stand above or
below one figure alone show which, and where three in four of them agree,
each caption goes to the nearest figure on that side. A caption set over
the figure it names, under the figure before it, is the lower figure's; one
under a plot and wider than it, set past the plot's axis values, is the
plot's all the same. Where the document shows no side, a caption goes to
the nearest figure. A caption stands across from its figure, sharing half
the narrower of their widths, within six of its lines, with no paragraph of
the page's prose between them; the lines a caption runs on into go with
it. A caption across from no figure (its chart was not found) stays where
it was, and a table's caption stays its table's.

**A list between a label and its drawing.** A list's items are never a
drawing's labels, so a label's reach must not pass over them. A section
head over its bulleted items, with the drawing below the items, is the
page's head, not the drawing's title.

**A picture with labels set on it is a figure.** A product's cutaway placed
as an image draws no shapes, and the names of its layers set over it
(`Heat Sealable PLA Layer`, `PLA Core`) would otherwise read as paragraphs. A
picture smaller than the page, with two blocks or more set on it, is a
figure of those labels and of any label hung over its edge, when every block
over it, even in part, is a label (fifteen words or fewer, no display type),
and the labels are set about it rather than one under another from a single
left edge. Its labels name the parts of the picture, as a raster chart's
values, a map's place names or a diagram's parts do. It takes nothing set
around it, so the heading under the picture opens the next section. None of
these is a figure:

- a photo with a title set over it, such as a slide's background;
- a brochure's background, with paragraphs that run on and off it;
- a signature under the line and the name it signs;
- a photo with only a credit in its corner.

A picture's labels still count toward the body size. A picture holds only
a few, and setting them aside could tip a slide deck's body to another
size.

**Headings near drawings.** A heading set larger than the body that the
figure took as a label keeps its type when the document sets headings at
that size and weight away from any drawing: `Operation` over a drawing of the
control it describes is the document's heading. A caption (`Figure 3: …`) is
the figure's, and so is a title set in bold at the body's size, which is how
charts set theirs. Inside the drawing, a numbered line such as `4-M4` is a
callout and not a section.

**Grids inside drawings.** Labels a diagram sets in rows can line up as a
table. A grid inside a drawing is emitted as that figure's text, not as a
`doco:Table`. So is a grid whose text lines up with no rules of its own,
when half of it lies on a drawing, and a ruled grid whose strokes stand at
many more places than its rows and columns: the outlines of a unit's
terminals, or a drawing's dimension and extension lines. So is a ruled row
or column holding nothing but three numbers or more that step evenly, by one
difference (`160 140 … 0`) or one ratio (`1 10 100`): an axis's scale ruled
by its tick marks. So are charts set side by side whose scales line up
into one grid (`240 | 240` down to `0 | 0`, each panel's title over its
scale): every column of numbers steps evenly and stands beside a chart's
plot. A table whose figures step so (a rank, a conversion) stands beside
no chart, and a grid holding a line of the page's prose is no chart's
axes. So is a dimension drawing's grid: its lines end in
arrowheads, two or more of them, and its cells hold callouts alone, such as
figures with their units (`83⁵/16"`, `(2117 mm)`), counts (`2X`) and datum
letters, with no words but units and a dimension's notes (`TYP`, `MAX`).
The drawing is a figure, and the callouts and view names around it are its
labels. A table drawn over a drawing, ruled at its rows and columns and
mostly filled, stays a table.

**A chart's scale beside prose.** Where a chart stands in one column and
the page's prose in the next, the prose's lines and the chart's scale can
line up into what looks like a table (`spending have deteriorated. Business
| 18.0`). Such a grid is no table: one column of it holds two numbers or more,
each level with a horizontal stroke that ends beside it (an axis's tick, or
a gridline's end), and one other column holds lines of running prose, with
nothing else beside them but columns of a word or a few (the chart's
legend). The prose is read back into its paragraphs, and the scale stays
with the chart. A table's figures stand between its rules, never level with
a stroke's end. A grid of a chart's own labels beside its scale, with no
prose in it, is kept as it is.

**A chart's scale taken into a column.** A scale set a few points from a
column's edge is read with the column, its ticks falling between the prose's
lines. Three numbers or more at one right edge, smaller than the text, that
step evenly with the column's lines between them are a scale. A scale is
read as one block after the paragraph it stood in, so its ticks neither cut
the paragraph nor are read as the numbers of a list. Charts set one under
another beside a column put their scales at one edge (`30 … −15`, then
`8 … 1`), and together the numbers step unevenly. They are read as the runs
that do step evenly, each with running prose between its ticks, counting up
the page and set at one pitch. A list's numbers count down the page. Two
runs that step evenly together are one axis whose labels end at two edges
(`0.5` beside `0.45`), and are read as one scale. Two charts' scales side by
side are read apart (`25 … 0`, then `1400 … 0`).
No grid is read across the edge of a box drawn round a drawing, a box
holding three shapes or more: a diagram's wire labels inside the box and a
note in the column beside it stand on the same baselines, and read as rows
they cut the note after its first line. A table's shaded label column holds
text, not shapes, and parts nothing.

**Body text size.** The size headings are measured against is taken from the
text outside drawings. A diagram's many small labels do not make every
caption beside it "larger than body".

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

~8 ms per document for the deterministic tier, ~2.4 ms per page for parsing
alone, ~420 pages/s. There is no model to load, so the first document costs
what the thousandth does.
