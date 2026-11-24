# Page furniture

Headers, footers, page numbers and watermarks. Detected and removed **before**
table assembly.

## Why it matters more than it sounds

Furniture that leaks into the body does more than add noise.

**It breaks tables.** On one regression document a leaked
`Acme Analytics, Inc. | AA00733622` landed inside a table cell, and the `|`
broke the Markdown column count on 2 of 47 rows. A table that was otherwise
correct became unparseable downstream.

**It corrupts NER.** That same footer contributes **15 phantom
`Acme Analytics, Inc.` organisation mentions**, and the watermark yields a
spurious person/PII entity from an embedded email address — when neither
is document content. An extraction pipeline feeding a knowledge graph
propagates both as facts.

**It gets absorbed.** Running after paragraph assembly instead of before means
a footer has already been merged into the last paragraph of the page, and
removing it then requires editing text rather than dropping an element.

## The signal: cross-page repetition at a stable position

A single page carries no evidence. `Page 13 of 15` at the bottom of one page
is indistinguishable from body text; the same string at the same y-position
across twenty pages is unambiguous.

So furniture is only identifiable **in aggregate**, which is why detection
takes the whole document rather than working page by page.

## Short documents

Repetition alone needs at least three pages, and three is too many for the
most common short document there is: a two-page data sheet whose letterhead
heads both pages, or a three-page one whose running head sits over the two
pages after its title page. Left in, the second copy opened a new section
halfway down the sheet, and everything after it was nested under a duplicate
of the title.

So a short document's running line is read from how exactly its copies
agree. It must be on every page, or every page but one, in the top or bottom
band, and its copies must match as only a template's do: the same text, size
and weight, at the same place to a tenth of their size. Body text that
happens to repeat on two pages does not land on the same point. A page
number may differ between the copies if it counts the pages and the line
says nothing else (`Page 1 of 2`); `Table 1` over one page and `Table 2`
over the next are headings.

A title page that sets the running head's words as its own title, larger
and in its own place, keeps its title. The copies at the running head's
place are furniture; the title is not. Nor are the same words elsewhere on
the page: a short document's running line was found as a line of its own on
every page it is taken from, so none of its copies merged into a block or a
table, and it is not scrubbed from them. A safety data sheet runs its
product code over its inner pages and prints it in its tables too.

The scrub that catches a header merged into a block on one page also leaves
a block alone that is all of the running text at another size, whatever the
document's length: a report's cover title is the running head of every page
after it.

```bash
fdoc dev furniture report.pdf
```

Measured on the regression document: 738 lines → 679 body lines, 59 removed.

## Watermarks

Away from the top and bottom of the page, repetition is not enough. A
templated document (a data-sheet family, a catalogue, an install guide that
repeats its figure in each language) sets real text at the same place on
page after page: a spec table's labels, a box's heading, the close of a
standard sentence. Taken for watermarks, these were deleted from every page,
leaving values without their labels and sentences without their ends.

A watermark also looks like one, and that is what is checked:

- **Laid over the text.** A `DRAFT`, a diagonal `CONFIDENTIAL` or an
  `Uncontrolled copy` stamp is drawn over (or under) the page's text, and
  the text beneath it changes from page to page. It is set apart from that
  text as a layer of its own: turned, much larger, or drawn before or after
  all of it.
- **Beside the text.** A note in a side margin (`For internal use only`, a
  `Downloaded from …` line turned up the edge, a catalogue's thumb index)
  shares no width with the page's other text.

A watermark laid over the text is taken out before lines are assembled, as
the run of glyphs it was drawn as. Left in, it shares the baseline of every
line it crosses, and its letters are sorted in among that line's words:
`imDpeller flangeR impeller gasAket`.

Template text does neither: it sits among the page's lines, at their size,
and nothing lies on it. A figure that repeats and overlaps its own labels
does so the same way on every page, and is drawn among the page's text, so
it is kept as well.

## Matching inside table cells

Removing furniture from prose is not enough — it also has to be scrubbed from
cells, and the match has to tolerate how the same text varies:

| kind | matching |
|---|---|
| constant footers | exact |
| page numbers | digit-insensitive (`Page 13 of 15` ≡ `Page 14 of 15`) |
| watermarks | prefix-matched, because they wrap differently across regions |

An independent VLM labelling of the same page agrees with these deterministic
furniture decisions line for line.

A match is cut from a cell only as whole words: no letter or digit may run
on into it, nor a number through a decimal or grouping mark. A short string
(three characters or fewer, or nothing but numbers, such as a figure's
callouts `1` to `7`) is cut only where it is the whole cell. Matched as a
substring, a callout taken for furniture took its digit out of every cell
that held it: `Serrer les vis à 30 po-lb (3,3 Nm)` read `0 po-lb ( , Nm)`,
and an OCR'd newspaper whose stray letters were furniture lost every `r`
and `e` from its table.

A page number alone has no text to match a cell's on. A grid that reaches
into the foot of the page takes its folio in, so a cell is emptied as the
page's number when it holds that page's own folio and nothing else, and
stands in the top or bottom band where page numbers are printed. Its width
is no evidence: matched by width alone, every cell as wide as the page
numbers was emptied, an accessory list's `CARTON COUNT | 1`, a catalogue's
column of I/O points (`10`, `14`, `32`), and the minimum and typical columns
of a datasheet's electrical tables, whose maximums then stood under the
wrong headings.

Paragraphs are scrubbed too, for a header or footer that merged into a line of
its own block on one page. There the match is held to where such a leak can
be: the start or end of a block that reaches into the page's top or bottom
band. A running header is often a phrase the document's sentences use, and
stripped wherever it occurred, a set of minutes headed `Federal Open Market
Committee` lost the committee's name from every sentence that named it.

Two running lines can merge into one block together, a chapter title over
its number, and the inner one stands at the block's end only once the outer
one is gone; a cell can hold a title block's `DATE: -`, where the `-` is a
pattern too, left whole only once `DATE:` is cut. So the patterns are tried
until none takes anything more, the longer first where two overlap (`DATE:`
before `DATE`). What is left does not depend on the order the patterns were
found in. Tried once each in an arbitrary order, the same document read
differently from one run to the next.

## What is not furniture

False positives worth knowing about, all fixed by tightening rather than by
adding signals:

- **A bare page number that prefixes a figure caption** is part of the
  caption, not furniture.
- **A short repeated string that is not marker-shaped** — a marker has to look
  like a marker, not merely be short.
- **A number alone that does not count the pages.** An install guide's step
  numbers, a figure's callouts and a chart's axis labels sit at one place on
  page after page, near the top or the foot, just as a folio does. A line that
  is nothing but a number is a page number only if it is printed once on a
  page and goes up from one page to the next by as many pages as lie between,
  on most of them: a guide in three languages numbers each from 1, and an
  unnumbered insert skips one.

## Where it goes

Detected furniture is dropped from every page rather than retyped as
`doco:FrontMatter`. The one exception is the running header and footer a
document opens and closes with, kept once; see
[below](#removed-from-the-body-kept-about-the-document). The text projection,
the graph and the offsets stay consistent: an element that does not exist has
no text and no offsets, and one that does has both.

## Model readings are scrubbed too

Furniture is stripped before lines become blocks, so a deterministic reading
never carries a running footer. A model reading is transcribed from the pixels
and always does — the footer is printed on the page, and a reader looking at
one page has no way to know it repeats on nineteen others.

Left alone, the same document comes out with a footer or without one depending
on whether a page happened to escalate, and nothing in the output says which
happened. So the list the deterministic pass built is handed to
[`arbiter::scrub_furniture`], which applies it to every element a model
produced, on both splice paths.

Line by line, because a page reading arrives as a whole page in one block.
Where a line had furniture removed and has no letters left, it goes entirely:
`12 MORGAN STANLEY WEALTH MANAGEMENT` scrubs to `12`, which is the folio, not
content. A line that merely *contains* a number is untouched — nothing was
removed from it, so there is nothing to judge, and that is the guard that
keeps a financial statement's figures intact.

[`arbiter::scrub_furniture`]: ../integration/escalation-tiers.md

## Removed from the body, kept about the document

Repetition is what makes a line noise inside the body, and exactly what makes
it identify the document. A controlled procedure — an SOP, a test method, a
form — puts its owner, title and number in a block printed on every page. On a
three-page one they repeat three times, which is indistinguishable from a
running header, and stripping them leaves the document anonymous: one test
method lost `CHURCH &`, `DWIGHT`, its title and its document number in a
single pass.

So every line is declared once on the document node as
[`doc:runningText`](../formats/doco.md), and the header and footer are also
kept once in the text, the way a Word file's are read: the header printed on
the first page, before that page's content, and the footer printed on the
last, after its content. These are `doco:Paragraph` elements whose `evidence`
is `header` or `footer`, with the box they were printed in, so a consumer that
wants the body alone can leave them out. A header line whose page number
changes is the first page's copy: `Form QA-12 Page 1`.

A running head that starts later in the document, such as a report's title
over its inner pages or the meeting date on its even pages, is not kept in the
text. Placed where it is first printed, it would stand between two pages'
text, and the document's first page usually says the same thing in its own
title. It is in the running text with the rest. Lines that name nothing are
left out of both: a folio identifies nothing about the document, and neither
does `Page 3 of 15`. Only a line whose number changes from page to page can
be one of those. A line the same on every page is kept however it reads: a
telephone number, a revision year or a form number is the document's own.

## The folio identifies the page

It does identify a page, and it is the one piece of furniture that does. A
chart assembled from a shuffled, duplicated scan carries its original page
number in every footer and nowhere else; strip the footer and the order is
gone. So the page number is kept per page rather than per document: each
entry of [`doc:pages`](../formats/doco.md) carries `folio`, the printed
number, where the document has one.

Which digits are the page number is decided by variation. A footer's digits
that change on nearly every page are the folio; the MRN and account number
beside them, which never change, are not, and a flow sheet's `16:00` row
label — which repeats at a stable position on hundreds of pages and takes six
values between them — is a clock. The page-number line that covers the most
pages supplies it. A page it leaves without one takes its folio from another:
a guide in three languages words its `1 of 3` three ways.

A line is a source only if its number is a page's. Digits that vary at the top
or foot of page after page are not one for that alone: a data sheet's spec
table opens each page with its largest drive (`26TB`, `16TB`, `8TB`), a parts
catalogue sets year ranges (`1991-1998`) at the foot several to a page, and a
translation of a declaration ends each page on an article's number. Each was
the folio of every page it reached. A page's number counts the pages, going up
by as many pages as lie between, or says it is one (`Page 352`, `3 of 15`).
The second is what keeps a shuffled chart's folios, which count nothing in the
order the pages now stand.

The folio is read from the page's edges over *every* glyph, not from the
lines left after grids took theirs. A form's key/value grid that runs down
into the footer band takes the footer's glyphs with it — the furniture pass
never sees the line, and the page had no folio although it printed one. The
edge bands are where pagination lives, and assembling them a second time is
cheap. `fdoc dev furniture` reports what the pipeline stripped and which
pages carry a folio.
