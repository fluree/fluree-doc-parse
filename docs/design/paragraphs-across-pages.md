# Paragraphs across page breaks

Status: built on branch pdf-paragraphs-across-pages. It is planned for the release after the next one.
The DoCO shape is confirmed (2026-11-28): two nodes, one per page, linked by
`doc:continues`.

A paragraph the page break cuts is read today as two paragraphs, one on each
page. Where the page sets its notes at its foot, the notes are read between the
two halves:

```
… The recycling of manufacturing scrap and end-of-life batteries is
increasing significantly as electric vehicle adoption

1 Source: https://www.sbir.gov/node/1656649

2 Source: https://pnecycle.com/news-room/

and demands for the power grid storage grow. …
```

Every format then reads a broken sentence, and a search or a model reading the
Markdown sees two sentence fragments with a citation between them.

## How often

Measured on about 5,000 PDFs (the held-out set, government reports, journals,
datasheets, the bench and the tough documents). A break counts where a page's
last paragraph ends mid-sentence and the next page's first element is a
paragraph opening in lower case:

| Kind of break | Documents | Breaks |
|---|---|---|
| The page's foot notes in between | 69 | 486 |
| Nothing in between | 275 | 3,325 |

Examples:
- DOE SBIR success stories: a block quote breaks at `A barrel of oil is`, and
  two sentences break at `electric vehicle adoption` and `By reducing
  materials`.
- Federal Reserve FOMC minutes: `…matter for economic decisions and` /
  `can affect the persistence of inflation.`, with the page's note on the
  Committee between. Some break inside a hyphenated word (`re-` /
  `strictive`).
- Beige Books, FOMC press conference transcripts, journal articles, annual
  reports and datasheets.

## The model

A paragraph's parts are linked as a table's are. Each part stays one element
on its own page, with its own box, since an element and its box sit on one
page. The later part's `continues` names the earlier part's `id`. This is the
field a table carried over a page break already uses (see
[Tables](tables.md#tables-split-by-a-page-break)).

## Detection

A paragraph on page n+1 continues the one on page n when all of these hold:

- The part on page n is the last body paragraph on its page. Only the page's
  foot notes, or nothing, follow it there; page furniture is already gone.
- It ends mid-sentence: in a word, a comma or a line-end hyphen, not in
  sentence-final punctuation, a number or a leader.
- Its last line is one the page break cut. The part is set in a column of
  text (two lines or more, or other text on its page set from its left
  edge), and its last line has no room left for the next part's first word,
  the word's width read at the line's average character's, give or take
  2 em. A folio (`v` over `vi`), a label (`ANNEX TABLES`), an address or a
  byline at a page's foot fails this.
- The part on page n+1 is the first body element on its page, and a
  paragraph.
- It opens in lower case.
- Both parts are set in the same type size, within 5%.
- The text flows on from one part to the other:
  - no text on page n stands in a column right of the earlier part;
  - no text on page n+1 stands in a column left of the later part;
  - the two columns have one measure, within 2 em.

  On pages of one column, that is the same column, and a quote set in from
  it carries on too. In a two-column paper, a page's text ends in its last
  column and goes on at the head of the next page's first. The design first
  asked for the same left edge on both pages, which turned away every cut in
  two-column minutes and journals; column flow replaced it (2026-11-28).

These conditions keep out what the measurement found that is no paragraph:
- a table of contents' leader line ending in a page number, before a page
  numbered `ii`;
- a two-column paper's foot line (`…Association for Computational
  Linguistics`), set under its first column while the text goes on in the
  second.

A figure, a table or a caption standing at the foot of page n or the head of
page n+1 is no foot note and no paragraph, so the parts are not linked across
it. A caption the page break cut goes on at the head of the next page in its
own size, and joins as a paragraph does; text after a caption is set in the
text's size, so the type size keeps a caption and the text apart. A running head on page n+1 is furniture and already gone; a section
title there is the first element, so nothing carries on.

A page's foot notes are what follows the earlier part on its page: each
opens on a note's mark (a figure, a raised figure, `*`, `†`, `‡`, `§`,
`¶`), stands under the part, and is set more than 5% smaller. Anything else
after the part means it is not the page's last text, and nothing is linked.
The notes move after the continued paragraph in reading order, after its
last part where it runs over several pages. A note belongs to the page, and
the sentence its mark sits in reads whole before it.

List items are not joined. An item a page break cut stays an item, and the
rest of it on the next page stays a paragraph, as before.

### A hyphen at the break

A part ending on a hyphen joins the next as a line's end does, by the same
word evidence (`HyphenJoin` and the document's own compounds, the ones the
line join reads):
- **A typesetter's hyphen inside a word** (`re-` / `strictive`): the earlier
  part's text ends on a soft hyphen (U+00AD), the mark a table cell's break
  uses until it is settled. The writers and the projection drop it and join
  the parts with nothing between, so the word reads whole (`restrictive`),
  and each part's offsets still cut its own text.
- **A hyphen the text keeps** (`long-` / `term` where the document sets
  `long-term` whole; `1-800-` / `222-1222`): it stays, and the parts join
  with nothing between.
- **A dash set apart, or a compound's first half left hanging** (`a pause -`,
  `wage-` / `and price-setting`): it stays, and the parts join with a space.

A part ending on a hyphen after a figure (`1-800-`) ends on no word, so it is
not linked at all; the rule above is what the writers apply to any linked
part.

## The writers

- **Markdown, XHTML, text:** the parts are written as one paragraph, joined by
  a space. A line-end hyphen is joined by the rule for a hyphen at a line's
  end (`re-` + `strictive` → `restrictive`; a compound's hyphen stays). This is
  how a continued table's rows run on with no second header.
- **DoCO:** one `doco:Paragraph` per part, each with its page and box. The
  later part has `doc:continues` naming the earlier, as tables do. The text
  projection reads the paragraph whole, so each part's offsets cut its own text
  out of the joined paragraph. The parts' offsets touch, a space apart.
- **Conformance:** the reference joins continued paragraphs as it joins
  continued tables, so a writer that runs them on conforms.

## Evidence the change must bring

- A corpus A/B, judged by class:
  - the notes case;
  - the plain case;
  - the hyphen case;
  - every rejected near miss (leaders, foot lines, a column change).
- Word and letter counts per document unchanged except for joined hyphens.
- Tough cases, each failing on main:
  - the two DOE SBIR fact sheets;
  - an FOMC minutes set, public, from a second producer;
  - a hyphen break.
  - Guards: a table of contents and a two-column paper.
- The bench doesn't move: its PDFs are single pages.

## Decided

- **DoCO:** two nodes, one per page, the later linked to the earlier by
  `doc:continues`, as a continued table's parts are. It was confirmed on
  2026-11-28. The alternative was one node spanning two pages, which would
  break "an element sits on one page".
- **The column guard:** column flow, in place of the same left edge
  (2026-11-28).
- **A hyphen at the break:** the soft hyphen, as a table cell's break is
  marked, and no field of its own (2026-11-28).
