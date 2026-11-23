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

Paragraphs are scrubbed too, for a header or footer that merged into a line of
its own block on one page. There the match is held to where such a leak can
be: the start or end of a block that reaches into the page's top or bottom
band. A running header is often a phrase the document's sentences use, and
stripped wherever it occurred, a set of minutes headed `Federal Open Market
Committee` lost the committee's name from every sentence that named it.

## What is not furniture

Two false positives worth knowing about, both fixed by tightening rather than
by adding signals:

- **A bare page number that prefixes a figure caption** is part of the
  caption, not furniture.
- **A short repeated string that is not marker-shaped** — a marker has to look
  like a marker, not merely be short.

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
does `Page 3 of 15`.

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
pages supplies it.

The folio is read from the page's edges over *every* glyph, not from the
lines left after grids took theirs. A form's key/value grid that runs down
into the footer band takes the footer's glyphs with it — the furniture pass
never sees the line, and the page had no folio although it printed one. The
edge bands are where pagination lives, and assembling them a second time is
cheap. `fdoc dev furniture` reports what the pipeline stripped and which
pages carry a folio.
