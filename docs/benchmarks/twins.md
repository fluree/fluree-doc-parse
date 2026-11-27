# Twins: a scan against its born-digital page

A scan should read as the born-digital page it was printed from. The twins
set holds OCR to that: each page is written as a born-digital PDF in the
standard fonts, and the same page rasterised, then recoloured or degraded.
fdoc reads the PDF by its text layer, at exact sizes and weights, and the
image by OCR, and the check counts the headings each reads. The pages have
nothing but size or weight to tell a heading from the body: headings set a
fifth or a third larger, not bold, or set at the body's size in bold; and
short lines at body size alone on their line, which must not read as
headings. With a text layer, fdoc reads nearly every heading and no short
line, so what the image misses is OCR's: the size and the weight it gives
the words it reads.

The set, the sets and how to run it are in
[`eval/twins/README.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/twins/README.md).

## What it says

Read by the built-in engine at 200 dpi unless the set says otherwise, three
faces (Helvetica, Times, Courier) at 10 to 12 pt. *Boxes* sizes each word by
its detector box and gives it no weight, as fdoc did before the engine
measured each word's ink; *ink* is the release. A page that sets a heading
or a short line twice counts each copy once. *Other* counts the lines read
as headings that are neither: a line of prose, or a heading whose text OCR
misread.

| set | one-word headings, boxes | ink | line headings, boxes | ink | short lines as headings, boxes | ink | other, boxes | ink |
|---|---|---|---|---|---|---|---|---|
| `plain-1.2`, 150 dpi | 56 / 88 | 88 | 59 / 106 | 106 | 1 / 173 | 1 | 2 | 0 |
| `plain-1.2`, 200 dpi | 50 / 88 | 88 | 73 / 106 | 105 | 1 / 173 | 0 | 0 | 0 |
| `plain-1.2`, 300 dpi | 23 / 88 | 88 | 61 / 106 | 106 | 0 / 173 | 0 | 3 | 0 |
| `plain-1.3`, 150 dpi | 70 / 84 | 84 | 90 / 103 | 103 | 2 / 168 | 0 | 1 | 0 |
| `plain-1.3`, 200 dpi | 70 / 84 | 84 | 90 / 103 | 103 | 0 / 168 | 0 | 1 | 0 |
| `plain-1.3`, 300 dpi | 59 / 84 | 84 | 92 / 103 | 103 | 0 / 168 | 0 | 5 | 0 |
| `bold`, 150 dpi | 15 / 93 | 21 | 2 / 106 | 28 | 5 / 180 | 0 | 1 | 1 |
| `bold`, 200 dpi | 0 / 93 | 87 | 1 / 106 | 102 | 0 / 180 | 0 | 3 | 1 |
| `bold`, 300 dpi | 0 / 93 | 87 | 0 / 106 | 103 | 0 / 180 | 0 | 7 | 1 |
| `blue-ink` | 62 / 88 | 88 | 90 / 106 | 106 | 6 / 173 | 0 | 0 | 0 |
| `red-ink` | 62 / 88 | 88 | 80 / 106 | 106 | 3 / 173 | 0 | 0 | 0 |
| `grey-ink` | 27 / 88 | 85 | 62 / 106 | 105 | 0 / 173 | 0 | 6 | 0 |
| `inverted` | 56 / 88 | 88 | 68 / 106 | 105 | 1 / 173 | 0 | 0 | 0 |
| `white-on-blue` | 55 / 88 | 87 | 72 / 106 | 105 | 2 / 173 | 0 | 0 | 0 |
| `yellow-paper` | 43 / 88 | 88 | 64 / 106 | 105 | 0 / 173 | 0 | 2 | 0 |
| `gradient` | 11 / 88 | 87 | 47 / 106 | 104 | 0 / 173 | 0 | 13 | 0 |
| `photo-bg` | 35 / 88 | 88 | 68 / 106 | 103 | 0 / 173 | 0 | 3 | 0 |
| `noise` | 61 / 88 | 86 | 70 / 106 | 105 | 1 / 173 | 0 | 0 | 2 |
| `blur` | 44 / 88 | 81 | 54 / 106 | 97 | 0 / 173 | 0 | 0 | 0 |
| `jpeg` | 56 / 88 | 88 | 68 / 106 | 106 | 7 / 173 | 0 | 1 | 0 |
| `fax` | 54 / 88 | 88 | 70 / 106 | 106 | 0 / 173 | 0 | 0 | 0 |
| `skew` | 39 / 88 | 88 | 52 / 106 | 106 | 0 / 173 | 0 | 0 | 0 |
| `underline` | 67 / 88 | 88 | 74 / 106 | 106 | 69 / 173 | 0 | 2 | 1 |
| `strike` | 56 / 88 | 88 | 69 / 106 | 105 | 2 / 173 | 0 | 3 | 0 |
| `boxed` | 73 / 88 | 81 | 56 / 106 | 102 | 27 / 173 | 3 | 3 | 3 |
| `superscript` | 74 / 88 | 84 | 72 / 106 | 104 | 20 / 173 | 1 | 7 | 4 |
| `italic` | 56 / 88 | 87 | 68 / 106 | 106 | 1 / 173 | 0 | 0 | 0 |
| `punct` | 48 / 89 | 88 | 62 / 103 | 103 | 0 / 168 | 0 | 2 | 0 |
| **all** | **1,322 / 2,468** | **2,357** | **1,734 / 2,956** | **2,844** | **148 / 4,845** | **5** | **65** | **13** |

The born-digital pages read every heading of every set but some of `bold`'s
(87 of 93 one-word headings, 103 of 106 lines), and two other lines as
headings there.

- **A box says a line's size to about 5% either way.** Its margin moves
  with the letters under it, the pixels its text spans and the room around
  it, and a heading set a fifth larger than its body has 4% to spare over
  the 15% a heading needs: by their boxes, 60% of the headings of
  `plain-1.2` read as headings, fewer the larger the scan (300 dpi reads
  about one word in four). A third larger, most do.
- **By their ink, a scan reads nearly as its born-digital page does.** The
  engine measures each word's capitals, ascenders and x-height in the image
  it read (see the [OCR words format](../integration/sidecar-formats.md)):
  every heading of `plain-1.2` but one, and of `plain-1.3`, reads as a
  heading, and one short line at body size of 1,023.
- **Bold.** A scan's bold is in its stems, which the engine measures too: at
  200 and 300 dpi the bold headings read as headings as on the born-digital
  page, where by their boxes none do. At 150 dpi a stem is a pixel or two,
  and most words are left without a weight rather than guessed.
- **Colour, a dark ground, a photo or a gradient behind, noise, JPEG, a fax,
  a turned page, rules and italic** leave it at 91% or more of each kind,
  blur the lowest; by their boxes, 12% to 85%, and an underline under a
  short line made it a heading 69 times in 173.
- **Marks under the baseline.** A comma, a bracket or a `Q`'s tail
  (`Total (USD)`, `Q1 Revenue`, the `punct` set) reaches under the
  baseline in a few columns of its word: measured from them, a short line
  at body size read a fifth larger, and 54 in 168 as headings. The
  baseline is the level a sixth of the word's columns stand on, and none
  does.
- **What is left.** A box drawn round a short line reads it as a heading
  three times, and lines whose text OCR misread (a box's side read as a
  letter, `Payment$`; a note mark read into a word) are most of the other
  lines.

## Mixed documents

A document can hold text of its own and pages that are only images: a memo
printing a scanned screenshot, a letter with a signed form scanned behind
it. A page read whole by OCR has no scale of its own: its type is as large
as the image was drawn, so its sizes say nothing next to the text's. The
mixed sets (`mixed-small`, `mixed-large`, `mixed-twotext`) put a page of
text and a page that is an image in one PDF. Counted over all their pages,
the text's and the image's:

| set | one-word headings | line headings | short lines as headings | other |
|---|---|---|---|---|
| `mixed-small`, the body read over every page | 64 / 91 | 84 / 108 | 34 / 183 | 154 |
| `mixed-small`, each page at its own body | 61 / 91 | 75 / 108 | 0 / 183 | 0 |
| `mixed-large`, the body read over every page | 59 / 59 | 62 / 67 | 3 / 116 | 13 |
| `mixed-large`, each page at its own body | 59 / 59 | 62 / 67 | 0 / 116 | 0 |
| `mixed-twotext`, the body read over every page | 78 / 118 | 96 / 144 | 51 / 238 | 228 |
| `mixed-twotext`, each page at its own body | 88 / 118 | 103 / 144 | 0 / 238 | 0 |

- **Read over every page, the body is a vote the image can win.** A page
  showing two pages at half size holds more lines than the page of text, at
  half its type; the body came out at the image's size, and the text's prose
  read as headings. The vote is a histogram's, by the quarter point: the
  same document could go either way with the image's sizes read a little
  differently.
- **Each page at its own body.** Where the document has text of its own,
  its body is that text's, and a page read whole is set at it by its own
  median, so a line stands out on its page or does not. Its headings are
  inside the document's outline: under the document's section open above
  them, never over the next one.
- **What is left** is the half-size image's own headings, a fifth larger
  than its 5 pt type: about half of them read as headings (34 of 64 one-word
  headings in `mixed-small`, and all of its page of text's 27).

The set does not test the general rules, which read its headings from a
text layer.
