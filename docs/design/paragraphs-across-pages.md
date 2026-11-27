# Paragraphs across page breaks

Status: design, not built. It is planned for the release after the next one.
The DoCO shape is to be confirmed.

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
- The part on page n+1 is the first body element on its page, and a
  paragraph.
- It opens in lower case.
- Both parts are set in the same type size and in the same column (the same
  left edge, give or take the measure's indent).

The last two conditions keep out what the measurement found that is no
paragraph:
- a table of contents' leader line ending in a page number, before a page
  numbered `ii`;
- a two-column paper's foot line (`…Association for Computational
  Linguistics`), whose next page goes on with a different column's text.

The page's foot notes move after the continued part in reading order. A note
belongs to the page, and the sentence its mark sits in reads whole before it.

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

## Open

The DoCO shape (two nodes linked by `doc:continues`) mirrors tables. It is to
be confirmed before building. The alternative, one node spanning two pages,
would break "an element sits on one page".
