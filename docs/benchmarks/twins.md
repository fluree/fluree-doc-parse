# Twins: a scan against its born-digital page

A scan should read as the born-digital page it was printed from. The twins
set holds OCR to that: each page is written as a born-digital PDF in the
standard fonts, and the same page rasterised, then recoloured or degraded.
fdoc reads the PDF by its text layer, at exact sizes, and the image by OCR,
and the check counts the headings each reads. The pages have nothing but
size to tell a heading from the body: headings set a fifth or a third
larger, never bold, and short lines at body size alone on their line, which
must not read as headings. With a text layer, fdoc reads every heading of
every set and no short line, so what the image misses is OCR's: the size it
gives the words it reads.

The set, the sets and how to run it are in
[`eval/twins/README.md`](https://github.com/fluree/fluree-doc-parse/blob/main/eval/twins/README.md).

## What it says

Read by the built-in engine at 200 dpi unless the set says otherwise, three
faces (Helvetica, Times, Courier) at 10 to 12 pt. *Boxes* sizes each word by
its detector box, as fdoc did before the engine measured each word's ink;
*ink* is the release.

| set | one-word headings, boxes | ink | line headings, boxes | ink | short lines as headings, boxes | ink |
|---|---|---|---|---|---|---|
| `plain-1.2`, 150 dpi | 58 / 88 | 88 | 69 / 106 | 106 | 1 / 173 | 0 |
| `plain-1.2`, 200 dpi | 52 / 88 | 87 | 78 / 106 | 106 | 1 / 173 | 0 |
| `plain-1.2`, 300 dpi | 25 / 88 | 88 | 69 / 106 | 106 | 0 / 173 | 0 |
| `plain-1.3`, 150 dpi | 71 / 84 | 84 | 90 / 103 | 103 | 4 / 168 | 0 |
| `plain-1.3`, 200 dpi | 70 / 84 | 84 | 92 / 103 | 101 | 0 / 168 | 0 |
| `plain-1.3`, 300 dpi | 61 / 84 | 84 | 95 / 103 | 103 | 0 / 168 | 0 |
| `blue-ink` | 64 / 88 | 87 | 94 / 106 | 106 | 9 / 173 | 0 |
| `red-ink` | 63 / 88 | 87 | 86 / 106 | 106 | 4 / 173 | 0 |
| `grey-ink` | 31 / 88 | 86 | 66 / 106 | 106 | 0 / 173 | 0 |
| `inverted` | 58 / 88 | 88 | 75 / 106 | 106 | 2 / 173 | 0 |
| `white-on-blue` | 57 / 88 | 88 | 82 / 106 | 106 | 3 / 173 | 0 |
| `yellow-paper` | 45 / 88 | 87 | 69 / 106 | 106 | 0 / 173 | 0 |
| `gradient` | 15 / 88 | 88 | 50 / 106 | 105 | 0 / 173 | 0 |
| `photo-bg` | 36 / 88 | 88 | 72 / 106 | 104 | 0 / 173 | 0 |
| `noise` | 62 / 88 | 87 | 81 / 106 | 105 | 2 / 173 | 0 |
| `blur` | 46 / 88 | 81 | 65 / 106 | 102 | 0 / 173 | 0 |
| `jpeg` | 58 / 88 | 88 | 83 / 106 | 106 | 9 / 173 | 0 |
| `fax` | 56 / 88 | 86 | 74 / 106 | 106 | 0 / 173 | 0 |
| `skew` | 40 / 88 | 88 | 54 / 106 | 106 | 0 / 173 | 0 |
| `underline` | 69 / 88 | 88 | 83 / 106 | 106 | 75 / 173 | 0 |
| `strike` | 57 / 88 | 88 | 78 / 106 | 106 | 2 / 173 | 0 |
| `boxed` | 74 / 88 | 83 | 65 / 106 | 105 | 37 / 173 | 5 |
| `superscript` | 75 / 88 | 82 | 76 / 106 | 106 | 25 / 173 | 1 |
| `italic` | 59 / 88 | 88 | 76 / 106 | 106 | 1 / 173 | 0 |
| **all** | **1,302 / 2,100** | **2,073** | **1,822 / 2,535** | **2,524** | **175 / 4,137** | **6** |

- **A box says a line's size to about 5% either way.** Its margin moves
  with the letters under it, the pixels its text spans and the room around
  it, and a heading set a fifth larger than its body has 4% to spare over
  the 15% a heading needs: by their boxes, 60% of the headings of
  `plain-1.2` read as headings, fewer the larger the scan (300 dpi reads
  one word in four). A third larger, most do.
- **By their ink, a scan reads nearly as its born-digital page does.** The
  engine measures each word's capitals, ascenders and x-height in the image
  it read (see the [OCR words format](../integration/sidecar-formats.md));
  581 of the 582 headings of `plain-1.2` read as headings and every one of
  `plain-1.3` but two lines at 200 dpi, with no short line read as one.
- **Colour, a dark ground, a photo or a gradient behind, noise, JPEG, a fax,
  a turned page, rules and italic** leave it at 92% or more of each kind,
  blur the lowest; by their boxes, 17% to 89%, and an underline under a
  short line made it a heading 75 times in 173.
- **What is left.** A box drawn round a short line still makes it a heading
  5 times in 173, and a note mark read into a word once. The two one-word
  headings of `superscript` the born-digital page does not read are its
  own: the mark ends their text.

The set does not test the general rules, which read every one of its
headings from a text layer; nor bold, which OCR does not see (a scan's bold
heading at body size is lost: see [Scanned PDFs](scanned.md)).
