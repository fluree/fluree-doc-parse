# Headings

The least certain part of the pipeline, and the one the [model
tiers](../concepts/escalation.md) most often improve.

## A priority cascade

Five detectors, tried in order. Each runs only where the one before it
declined, so the [`evidence`](../concepts/provenance.md) value on a heading
tells you how far down the ladder it came from.

| order | detector | evidence | basis |
|---|---|---|---|
| 1 | document title | `title` | a one-off style in the first blocks of page 1 |
| 2 | outline match | `outline` | the author named this heading |
| 3 | numbering | `numbering` | `5.3.1` states its own depth |
| 4 | weight | `bold` | bolder than body text, at the body's size |
| 5 | font size | `font-size` | larger than body, in a repeated size tier |

`font-size` is the weakest claim in the set — it is last because it is the one
most easily fooled by emphasis, captions and pull quotes.

Bold set larger than the body is ranked by its size, not given `bold`'s one
level. A part's 12pt bold label over 10pt bold heads stays above them, as it
does where the face's weight cannot be read at all.

A heading's weight is its leading run's when it ends on a bracketed note
set lighter. `raft-R-mate® Attic Vents (Ventilation Baffles)`, bold up to
its note, ranks with the other 10pt bold heads, not below them. A sentence
that opens on a bold name and runs on unbracketed keeps the weight of the
rest of its line.

A title must be four characters or more to be read as one on its shape
alone. Shorter lines are a word or two of a sentence, a page's letter, or
`No`. An acronym of two or three capitals on a line of its own (`FCC` over a
guide's compliance notes) is a heading where it is set exactly as one of the
document's headings is: the same size, weight and case, read on the same
evidence. It takes that heading's level.

Where the body itself is set bold, weight carries no information, and no
heading is read from it. Whether it is comes from the glyphs: the size most of
the document's letters are set at, and whether most of them are bold, its
tables' text counted with its paragraphs'. The blocks the tables leave are no
measure of it. On a form they are mostly its bold labels (Schedule 1 to
Form 1040 has 9 bold lines of 16), and on a financial statement page they are
its bold titles.

Weight and size never make a heading of these:

- **A caption** (`Figure 5-1. Low-Level Output Voltage`, `Table 4-1. Pin
  Functions`), which is the figure's or the table's.
- **A field:** a label ending in a colon with its value set beside it on
  its row (`Color:`), or after it on its line at the body's size, the
  value a figure or short and holding one (`CRI: 90+`, `Hours: 8:30 am -
  6:00 pm EST (Monday - Friday)`, `Initial Closing Date: May 15, 2019`). A
  title with a subtitle states no figure, runs on past a value's length,
  or is set larger (`Policy Rules: Limitations`, `Part II: Application of
  Section 67 Limitation to Specific Deductions`, `Current Situation:
  2001-2019` at 14 pt over 10).
- **A value under its label:** a line in a lighter weight set at its bold
  label's size on the label's next line, alone or with more such lines
  (`K25W951` under `PRODUCT NUMBER`, a company's name and address under
  `MANUFACTURER'S NAME`). A section under its title is set apart from it
  by size or a gap.
- **A label over its value:** a bold label with a single short line under
  it, set lighter at its size within an em, holding a figure or an
  answer, and followed by the next label or nothing (`Date of
  Preparation` over `Jun 5, 2026`, `Photochemically Reactive` over `No`).
  A section's heading stands over sentences, or over more lines than one
  (`Power and Ratings` over `120 V~ 60 Hz` and the rest of its ratings).
- **A stack of labels over values set in:** bold labels of a few words,
  set alike from one edge, each over its value. The value is set lighter
  at the label's size, within an em under it, set in from it by half an em
  or more, one to three lines, and no sentence (`PRODUCT NUMBER`,
  `PRODUCT NAME` and `MANUFACTURER'S NAME`, each over its value, at 9.6pt
  over an 8pt body). A single bold head over text set in is a heading as
  often as a field, so the stack is what tells them apart. A numbered title
  is a heading.
- **A form's label** ending in a colon or `#` (`Location:`, `Model #:`),
  where only its size would make it a heading: the blank beside it is
  where its value goes. Bold too, when it heads a box left blank
  (`APPROVALS:` and `AVAILABLE AT:` side by side over a sheet's footer boxes
  for a dealer's stamp). That means two things:
  - Nothing on its page reaches below it over its width, or is set in from
    it as a list's items are under their lead-in.
  - Nothing on its row to its right runs on from it, apart from another
    such label. An article's `FOURTH:` beside its text keeps that text.

  This is judged on the final elements, where tables and figures stand
  under the labels that introduce them.
- **Bold lettering:** bold set at three fifths of the text or smaller. The
  text is measured both by the document's body and by the page's level
  lines, and the smaller of the two is used. A watermark's turned letters
  can outnumber a page's lines and set the body at their size, and a title
  slide sets its own lines larger than the document's text. Examples are a logo's `IN
  THE USA` at 3.8pt over an 8pt body, a chart's labels and a figure's
  callouts. Real heads set small are still set as text is read, at seven
  tenths of the body or more.
- **A form's grid and line captions:** column keys and answer heads, and
  nothing else (`(a) (b)`, `Yes No`), and a line's caption between its
  letter and its number (`b Refunded 37b`).
- **A sentence that refers elsewhere:** `For Paperwork Reduction Act
  Notice, see your tax return instructions.`, a form's footer. So is an
  instruction opening on `See` and what it sends the reader to: `See the
  separate instructions.`, `See page 2`. A title that mentions the
  instructions keeps it in brackets, `Other Information (see
  instructions)`, and stays. `See also` heads further reading and stays.
- **A form's number or edition:** `OMB No. 1545-0074`, `Cat. No. 11358Z`,
  `Schedule D (Form 1040) 2013`, `Form 8949 (2025)`. A part the form names,
  `Schedule K`, has no edition beside it and stays.
- **A label in a form's header band**, the grid not read as a table: bold
  at the body's size or smaller, with short labels or figures beside it
  within a line above or below. It needs labels on both sides, three or
  more in all, or five or more on one side for the last column's head.
  Columns' heads stack over their notes and start at different heights:
  `Adjustment, if any, to gain or loss` over `If you enter an amount in
  column (g),` beside `(e)` and `(h)`. A statement's row label before its
  figures is one too. Not a label:
  - a heading with a paragraph, a long line or a list's item directly under
    it, as a heading in one of a page's columns has, beside the other
    columns' heads (`PVC and phthalates`, a spec sheet's `Performance`
    over its bullets);
  - a slide's card titles, which are set larger than their text;
  - another column's bullets, which don't count as labels.
- **A chart's axis title:** a line set at an angle to the page's text, up a
  plot's side (`Concentration of PFHpS, in ng/L`), or set directly under a
  row of three or more bare values that reaches across it, its scale (`I -
  Output Current - A` under `0 0.5 1 1.5 2`). A chart drawn as vectors
  keeps its labels among the page's text when no figure region takes them.
  On a page turned as a whole, the headings turn with the text and stay.
  A legend's entries and a curve's labels are the figure's to take.
- **A table's column head or stub, the table not read as one:** a line
  standing within two and a half ems over a line of bare figures (`Total
  assets, net` over its row's figures), or level with other short cells of
  which one stands over such figures (`95%CI` beside `95% CI` and `%`). A
  slide's card titles stand side by side too, but over text, not figures.
  Where the table is found, these cells are its headers.
- **A table's column head left out of the table:** a short line, eight words
  or fewer, set on a found table's top edge, within a line of it with
  nothing between, and inside its width. It must also sit over the table's
  value columns (past its middle), or on a row with two or more of the
  table's other heads. Examples are `Total SOMA` over two years' columns,
  `Item` over a stub on the row of the period heads, and `2008 vs. 2007
  2007 vs. 2006` over a change table's columns. These stay headings:
  - a table's title, set from the table's left edge or centred over it;
  - a section's heading over a table, which starts where the text does;
  - a form's part title beside only its `Part I` label;
  - a banner over its own paragraph, with the table after that.

  This is judged on the final elements, where the tables are.
- **A signature block's party:** a role or a name with a line to sign on
  just after it (`INVESTOR:` over `Acme Fund, LP` over `By:`, a name over
  `(Signature)`), and a signature page's label (`Signature Page to Voting
  Agreement`). Agreements set their parties bold, a dozen to a page.
- **A letterhead:** the sender's name, at the head of the first page, set
  over its address and numbers, directly or over a line or two (`TRUE
  MANUFACTURING CO., INC.` over `U.S.A. FOODSERVICE DIVISION` over its
  street, phone and web address).

A row of short blocks side by side was tried as a third test, as a table's
header cells (`TYPE`, `NAME`, `DESCRIPTION`), and dropped: a slide's card
titles stand side by side the same way (`Our Purpose`, `Our Mission`).

A form's text and a line that stops mid-phrase stay headings until runs of
bold heads have been judged, and are read as text after. A run of bold lines
that heads nothing, or stands three deep, is text on the evidence of the whole
run; taking one of its lines out first would leave the others standing as
headings, a table's cells among them.

No heading stops mid-phrase, on an article, a conjunction, `of` or `not`: a
line that does runs on into the next, as a form's numbered caption cut at
its first line (`5 Expenses recorded on books this year not`) or a side
label set a word or two a line (`Tax and` over `Credits`). Only lowercase
words count, so `Part A` and `Schedule A` stay titles.

A heading holds at most 12 words. The limit rises to 20 words, on at most
two lines, in two cases where more than its shape vouches for it:

- a numbered title set larger than the body: `3. Perspective of supply and
  demand balance of wood pellets and cost structure in Japan`, 13 pt over
  11 pt;
- a line set at one and a half times the body or more: a slide's statement
  or a page's display title.

A long numbered line at the body size is a list's item or a note's, and a
long bold line is a sentence, so both keep the 12-word limit. Even when
large, a long line must also be shaped as a title. These are not:

- a line ending in `.`, `?` or `!`;
- a line holding two or more commas, such as an author list;
- a bare-number label, such as a form's `12 Did you explain…`;
- a formula holding `=`;
- a line that opens with a bullet.

## The body size

"Larger than body" is measured against one body size per document: the size
set on the most lines. Drawings' text is left out of that count: the blocks a
figure region holds, its labels and the text inside its drawing, are found
before headings are. A wiring diagram is dozens of one-line labels, and on a
submittal sheet 64 of them at 4.25pt outnumbered a 25-line column at 10pt. The
body came out at the labels' size and every photo caption became a
`font-size` heading. With nothing left once drawings are set aside, every
block counts.

Tables the table pass did not take are still counted. In a catalogue whose
specification tables outnumber its prose, the body still comes out at the
tables' size.

A document whose prose runs to fewer than eight lines has too little to say
its body by: a page that is a label, a title and a folio over a table. Its
prose alone counts the title's two lines against one of each other size, so
the title came out as the body and was no heading. There the tables' text
counts too, each of their lines once at its size.

## Headings beside drawings

A heading inside a figure region yields to it when its evidence is
typographic (`bold`, `font-size`): chart titles, legends and axis labels are
set larger or bolder than body text. Two exceptions:

- A `font-size` heading the figure took from beside its drawing keeps its
  type when the document sets headings at that size and weight away from any
  figure. `Operation` over a drawing of the control, or a side-head level with
  a chair drawing's top, is the document's. A caption keeps yielding, and so
  does bold at the body's size, because that is how charts set their titles.
  A report whose other charts are pictures sets those titles away from any
  region.
- `numbering` and `sequence` evidence yields too, but only inside the drawing
  itself, where `4-M4` is a dimension's callout.

## Where a weight comes from

A glyph's weight is its font's, read once per font in this order:

1. **A weight word in the font's name**, in the PDF's `/BaseFont` or the
   program's own: whole words anywhere (`TimesNewRomanPS-BoldMT`,
   `ArialBold`), abbreviations and German names in the style after the
   family (`HelveticaNeueLTStd-Bd`, `-Md`, `-Lt`, URW's `-Regu` and `-Medi`,
   `Sohne-Halbfett`, `-Kraftig`). Abbreviations are read only there, so a
   family's own name is never one.
2. **The descriptor's `/FontWeight`.**
3. **The program's weight class**, for TrueType and OpenType programs.

The name goes first because, in the disagreements on bold looked at across
a corpus of 7,000 documents, the name is the source that matches the page:
a subset of `NimbusSansL-Regu` whose program states 700 set a whole data
sheet in bold, `CooperHewitt-Thin` declares 701, `Frutiger-Black` 400. The descriptor goes
before the program because it describes the face as drawn: Word's
`Arial,Bold` embeds the regular program and states 700.

Then two document-wide passes:

- **Mediums are weighed, and faces with no weight where the body face
  states none,** against the body face, the one setting the most letters:
  bold when their ink covers 15% more of their glyph boxes than the body's,
  or, for a face setting too few letters to trust its ink, when its
  descriptor's stems are 1.35 times the body's. Letters alone are weighed,
  since figures and signs have ink of their own, and only the letters of a
  face read bold this way are bold: its figures are left with no weight,
  which neither makes a line bold nor keeps one from being. Kept medium,
  they outvoted the letters of a header cell (`WD8005FRYZ`); read as bold,
  they made bold lines of a table's `RCR: 0` and a paper's `doi:` line. A
  medium is weighed because
  its look depends on the family: Söhne's Kräftig is set as the bold beside
  Buch, as heavy as a semibold, where other families' mediums sit close to
  their regular. A face with no weight beside a body whose weight is stated
  is left alone: it is mostly a chart's, a logo's or a signature's, and
  weighing those made bold headings of signatures and axis labels. A face
  named elsewhere in a silent document, such as a repository's cover page
  in Frutiger Bold on a paper whose own faces state nothing, does not stop
  the paper's faces being weighed. Nothing is weighed against a light or a
  heavy body.
- **A light body lifts every weight.** A document set in Light (300) uses
  Medium (500) as its bold, so its weights are raised until the body reads
  400: the Medium reads 600, bold. The lift is one step at most: over a
  Thin body (200), a regular face is heavier without being bold, as a
  sheet's tables set in Helvetica beside its text in Helvetica Neue Thin.

## Bold, throughout

`bold` reads a line's weight, so a line must be set in bold, not hold some:
most of its inked glyphs bold, and its first and last words with letters
bold too. Three things make the line test stricter than a glyph vote:

- **A face that declares no weight counts as regular.** Many documents
  declare a weight for the bold face only. Counting only the glyphs that
  declare one made a run-in label (`PUMP: With stainless steel pump…`)
  most of its line.
- **A label can fill most of its line** (`MICROCOMPUTER CONTROL SYSTEM:
  Hobart`) and still end in the plain text it introduces, or open mid-line
  (`Sani-Dwell. Chemical Sanitizing…`). The first and last words decide.
- **Only words with letters decide.** A bold face's figures and signs often
  come from a face that declares no weight (`E3/E3x`, a chart's
  `October 2020`), and a heading may be numbered in another face (`3.2`).
- **The last word is the last before a gutter.** Words past a gap the line
  pass parts columns at are not the heading's own. The line pass measures
  that gap past a space that puts no ink down, so a heading no longer runs on
  into the next column's plain text (`9.5. Adapting to the New Normal:
  Changing`, then across the gutter `demand during COVID-19;`). A line it
  keeps whole across a gap, a number set before a tab (`3.1 ⇥ Results`), is
  still read to the gap.

Block assembly then treats the line that opens on a bold label as the start
of a paragraph, and a line that ran out of room as continuing into the next
whatever the weights, so a run-in paragraph reads whole and bold words
wrapped onto their own line stay in their sentence. A wrapped line goes on
at its column's left edge, at most a first-line indent left of the line it
wraps from, so a line starting far to the left of a full line is in another
column and begins its own block. Centred and right-aligned lines are exempt:
they start where their length puts them.

## Bold lines that head nothing

A section head is followed by what it heads: text, a table, a figure, or a
section under it. Once the page's tables and figures are placed in reading
order, `bold` headings at one level that follow each other with nothing
between them are looked at as a run. Three kinds of run are read as text:

- **A run that heads nothing.** The end of the document follows it, a bold
  heading above its level does, or a heading set larger, by its size or as
  the title: a bold line at the body's size ranks below any of those. This
  is the name and the role that close minutes or a letter (`Joshua Gallin`
  over `Secretary`), a slide's kicker over its title (`LEVEL 2 · DERIVED`),
  or a form's label over a larger line. A label naming a part of the
  document (`EXHIBIT B` over its title) stays, and so does the document's
  first heading, which may be its title over a larger date.
- **A run set as a row.** Two of its lines sit side by side, so the run is
  read across. This is a table's header cells left out of its grid, or an
  answer scale (`OFTEN`, `SOMETIMES`, `RARELY`, `NEVER`). Card titles set
  side by side on a slide are never a run, because each is followed by its
  own text. A division's label set beside its title (`Part I`, then
  `Annual and Monthly Contribution Amount` on the same line) is one heading
  in two blocks, not a row.
- **Three lines or more set one under another.** This is a list of names, a
  chart's legend or a map's labels, or a form's labels. The last line is
  followed by text and stays a heading.

A bold line set under a paragraph at its leading, with another bold head
after it, is that paragraph's last line set in bold, and reads as text
before any run is formed (`KEEP OUT OF REACH OF CHILDREN` closing a caution,
over `TRADEMARKS`). A heading stands apart from the text above it.

Two bold lines one over the other, followed by text, both stay. A section
that opens on its first subsection (`Materials and Methods` over `Apiary
selection`) is set that way as often as one label is set over another.

Only `bold` evidence is judged. A numbered, outlined or larger heading says
more than weight does. A bold line's level says nothing beside a numbered
or outlined heading's: a bold head over its outlined subsections (`Sectors
of primary focus` over `Energy`) has a deeper level number than they do. A
heading set larger does outrank it.

Two headings of one level and kind are one where the second goes on in
lower case from where the first starts, or where the two are centred on one
axis a line's spacing apart: a subtitle over two centred lines
(`Poly-Lactic-Acid` over `FDA Compliant for direct food contact`). Lines
set from one left edge, each opening with a capital, stay two headings, and
so does a division's label over its title.

## Numbering that is not an outline

A bare integer — `5 Results`, `2 — Never` — is the weakest numbering form,
and a questionnaire is made of it: every item answered with a code,
`0 — Never`, `2 — Somewhat agree`, twenty-odd to the page, each set larger
than the item it answers and numbered like a section.
Three tests keep them out, none of which touches `5.3.1`:

- **A numbered title occurs once.** An answer given more than once in a
  document is a code; so is every line labelled with a bare number that
  names two *different* titles (`1 — Yes`, `1 — Supplier on file`).
- **Sections are coarse.** A page holding more than ten bare-number
  candidates is a coding scheme, whatever each line looks like on its own.
- **Sections count from one**, and a number set as a superscript is a
  footnote's: `95 Ibid.` opens with its reference mark.

A block that carries a list marker is a list's item, and its number counts
off from the item before it, so numbering evidence is never read from one.
The same holds for a numbered list whose numbers are set in the items' own
text, which block assembly gathers into items before headings are read (see
[Numbered lists](../inputs/pdf.md)). Such an item is never a heading on any
evidence but the outline's.

A numbered line set below the body's size, under 0.85 of it, is a note:
a data sheet's `1) Feature only available with …` at 5pt under 7pt text, a
reference list set small, a chart's numbered title, a table's `(1)` notes.
Read as a numbered heading, such a footnote opened a level-1 section that
the rest of the document fell into. Lines set in capitals are the exception.
An agreement sets its sections in small capitals over clauses at the body's
size (`6. AFFIRMATIVE COVENANTS.` at 8pt over 10pt text), and a note is set
in the text's case.

Two label shapes look like numbering and are words:

- **A number hyphenated into a word**: `3-Way Wiring`, `2-Pole`, a
  drawing's `4-M4` hole callout. A dash separates a label from its title only
  with a space after it (`02- Prepare Your Content`).
- **Initials**: `U.S.A. FOODSERVICE DIVISION`, `U.S. Department of…`.
  Lettered numbering puts a letter at its first level only (`B.1`,
  `A.2.3`), so a label made of single letters is an abbreviation, unless it
  opens with a roman numeral (`I.A`). A lone letter with a dot, `B. Scope`,
  is a section only where a neighbour in its sequence, `A.` or `C.`, opens
  another block in the document. Otherwise it is an initial (`J. Example`).
  `I.`, `V.` and `X.` keep their roman reading alone.

## Addresses are not titles

A line that ends in a web address, a domain, an e-mail address or a phone
number is never a heading, however it is set. Examples: `seagate.com`,
`Learn more at thermofisher.com/centrifuges`, `For technical service call:
1-800-248-9606`. Data sheets set these bold or large at the foot of a page,
and as headings they open a section that the legal text or the order table
below falls into.

Nor is the line an address opens with, a company's or a person's name set
bold over it: `Owens Corning Insulating Systems, LLC` over the street and
over `1-800-GET-PINK® www.owenscorning.com`, the lines set flush under it,
each within a line and a half of the last, reaching one that closes on a
phone number or a web address within three lines.

A bare domain needs a lowercase host and a common top-level domain, so a
name such as `ASP.NET` is not one. A phone number ends in a group of three
digits and a group of four, or opens with `+`. A date, a standard's year
(`2043-2013`) or a count (`COVID-19`) fails that test. An outline entry can
still name such a line as a heading.

## An article's front matter

An article's first page sets its authors and its publication record by the
title, often bold or larger than the text. Neither is a heading.

- **Authors.** A line of people's names, each of two to four words, with
  the marks that point to their affiliations: `Roberta Lattanzi 1, Silvia
  Rief2, … and*`, `Hye Chong Hong and Ari Min *`. Two names or more need a
  mark. One name needs an affiliation's number with a corresponding
  author's symbol (`Mariana Spetea 2*`) or a list of affiliations (`1,2`).
  `Study 2`, `Model Year 2020` and `Posted Transactions*` are headings
  shaped the same way. A line that opens with the marks of the name
  before it carries the list on (`3,4, Steve W. Lindsay5‡, …`).
- **The publication record.** A line that opens with one of its labels,
  `Received`, `Accepted`, `Published`, `Editor`, `Citation`, `Copyright`
  and the like, then gives a value after a colon or a date:
  `Received September 7, 2009; Accepted …`, `Editor: Dana Davis, …`,
  `Copyright © 2023, …`. An editor's label set alone over the name
  (`Academic editor`) is front matter too. The word alone, `Funding` or
  `Copyright`, can title a section and stays a heading.

## The outline tree

**The signal no benchmarked engine uses.** A PDF's outline (bookmark) tree is
author-provided and explicitly hierarchical: where a block matches an outline
title, its level is taken directly rather than guessed.

This was verified against the alternatives: a source-level check of the other
benchmarked engines found no outline-tree usage at all — one never greps for
the outline dictionary, and another has its own outline-derived
table-of-contents processor commented out of its own pipeline. It is
near-ground-truth hierarchy, free, and sitting unused in the file.

```bash
fdoc dev outline report.pdf     # what the document declares
fdoc dev headings report.pdf    # what we detected, and on what evidence
```

## The document title

A title is often a one-off style, which means it cannot form a repeated
font-size tier and the generic detectors miss it. It is recovered first, from
a bounded look at the first few short blocks on page 1, stopping once prose
begins and requiring prominence, vertical isolation or a contents label.

Worth **+0.048 MHS**, recovering eight zero-or-low-scoring documents with no
regressions.

The title is the largest of those blocks, not the first: a cover often sets
its publisher, a category or a tagline above the title in smaller type
(`Scalable. Responsive. Innovative.` over `Exos X16`). Such a kicker, set
directly above the title with no text between them, is not a section of its
own either. As a heading it ranked below the title, so it opened a deeper
section that closed at the title with nothing in it. It is read as text,
where the page sets it. Journal banners (`Open Access`, `RESEARCH
ARTICLE`) are kickers too.

Reading order is not the page's order of size, so the first blocks can miss
the title:

- **A title read late.** A sidebar read first can hold the first large
  line: `Highlights` at 14pt, before the 17pt product title at the right.
  The page's largest title-shaped line in its upper half is the title when
  it is over a tenth larger than the first blocks' pick. Where the first
  blocks hold no title at all, a large line further down is a section's or
  a statement's, and stays one. The exception is a title set for display
  across the page: at 1.5 times the body's size or more, the page's largest
  type, and at least half the text's width. An article's title sits under a
  banner set across a row, `RESEARCH ARTICLE` at the left and `Open Access`
  at the right, and under its citation and DOI. A narrow line set large is
  a badge or a callout, such as a model's number at the right of a sheet's
  head.
- **A long title.** A title set for display may run to six lines and thirty
  words, as an article's states its finding (`An internal deletion in MTH1
  enables growth on glucose of pyruvate-decarboxylase negative, …`).
  Otherwise more than three lines is prose, unless a cover stacks a word or
  two to a line.
- **Two candidates of one size.** The one a column further left (by more
  than a quarter of the page's width) is the title. A category strip at the
  right of the header (`Wallbox Wiring Devices`) is as large as the title
  under it at the left. Otherwise the earlier one wins, so a title keeps
  its same-size subtitle.
- **A field.** A line at the body's size spread wider than its words could
  run, a label and its value set apart across the page (`Part No.` …
  `675 8126H250`), is never the title.

A division's label is not a kicker. A division word and its number, a
roman numeral or a letter (`Chapter 3`, `PART II`, `Appendix B`) is a
heading at the title's level. Joining it into the title as one heading was
measured and read worse: the benchmark keeps `Chapter 3` and its title as
two headings.

## Table of contents suppression

Headings after a contents marker on the same page are suppressed — the entries
below it are a table of contents, not sections. Without this a TOC produces a
full set of phantom headings that also happen to match the real ones.

## What was tried and rejected

Recorded because each is plausible enough to be proposed again. Full detail in
`eval/MHS_ANALYSIS.md`.

**Isolation as a signal** — the idea that a heading carries more whitespace
above it than the modal leading. It was ranked the strongest untried idea:
`block.rs` already computes the gap, and the cue is genuinely orthogonal to
size, weight and case.

Implemented as a last-resort detector, gated to documents whose typography
offers nothing else, requiring the gap above to exceed 1.6× the median and the
gap below by 1.4×. It fired on 10 documents, 13 blocks. **None of the 13 was a
heading** — they were figure captions, source notes and running heads. MHS
0.704 → **0.621**.

**All-caps detection** — rejected twice, on measurement both times. The second
attempt added a `title_like` gate on the hypothesis that the gate was why it
failed the first time. It was not: MHS 0.704 → 0.678.

## What MHS does not measure

Worth knowing before optimising against the benchmark: its evaluator builds a
*flat* section tree, and all 193 ground-truth headings across 107 documents
are `#`. Heading **level** is therefore unmeasurable there, and prose text
between headings is scored alongside the headings themselves.

Do not tune levels against that metric. The levels exist because
`doco:SectionTitle` carries a real one that consumers need — a product
judgement, not a benchmark result.

## Which bookmark is this heading?

An outline entry is matched to a heading by normalised title, and a title is
not a key. One benefits handbook carries **122 duplicated titles among 447
bookmarks** — "Overview" appears under four parents at four different depths,
and the text alone cannot say which one a given heading is. Keeping the
shallowest was the old answer: safe, and wrong three times out of four.

Every entry now carries where it points, and the heading's own page decides.
Nearest page first, then nearest stated position.

Most files write `/Fit`, which names a page and no position — of two documents
checked, 456 of 456 bookmarks resolved to a page and none to a position. So
the page carries this nearly always, and that is already enough: sections
repeat across a book, not within a page.

A destination may be written out or named: layout programs write the name of
a destination (`den24-0031_Layout.indd:Appendix 1:309`), kept in the catalog's
`/Dests` name tree or, in older files, its `/Dests` dictionary. Both are read,
so a named bookmark lands on its page like any other.

A bookmark heads one place: the copy of its title nearest where it lands, by
page, then stated position, then the larger type, each copy taking one
bookmark. A running head repeats its section's title at the top of every page
the section spans (`Appendix 1. Supplemental Figures` on 32 even pages of a
USGS report), and every copy matched the bookmark. The copies no bookmark took
that stand where another such copy stands on another page are the running line
itself, and no heading by weight or size either.

Entries that resolve to nothing keep the old rule. A bookmark naming no
destination cannot be placed, and promoting is still safer than burying a real
section.


### An outline that names files

Some PDFs are merged from files, and their outline names the files, not
their sections: `01 - beigebook - Boston_pdf_final_January_2023`,
`20250728.PressRelease.ImplementationNote.pdf`. No entry's title is printed
anywhere in the document, so no heading claims one, and levels come from size
alone. A 36pt summary outranks each district's 18pt title, and the districts
nest under it.

Each entry still lands where its file starts. So when none of the outline's
titles is printed, the topmost heading on each entry's page (at or below
where the entry lands) opens a part at the entry's level, with `outline`
evidence. There are three exceptions:
- An outline that prints any of its titles is the author's, and an entry it
  does not print is left alone.
- An outline whose entries land on pages next to each other (`Slide 1`,
  `Slide 2`) marks pages, not files.
- The page that carries the document's title opens the whole document.
