# Twins: born-digital pages and their images

What OCR is responsible for, a scan should read as its born-digital twin
does: the same headings, and no others. The set is a born-digital PDF per
page and the same page rasterised; the check converts both, the PDF by its
text layer and the image by OCR, and counts the headings each reads.

```
uv run eval/twins/make_twins.py                     # the set, into eval/twins/corpus/ (not tracked), about a minute
cargo build --release --features ocr -p fluree-doc-cli
eval/twins/check.py --models DIR                    # every set, about twelve minutes at -j4
eval/twins/check.py --models DIR --sets plain-1.2 fax
eval/twins/check.py --models DIR --baseline eval/twins/baseline.json
```

`DIR` is the OCR assets directory (`--models`, or `FDOC_OCR_MODELS`), and
`--fdoc` (or `FDOC_BIN`) checks another binary. The numbers are on the
[twins page](../../docs/benchmarks/twins.md).

## The set

Each page is Letter size in one of the standard 14 fonts (Helvetica, Times,
Courier), at 10, 11 or 12 pt, three draws each: paragraphs set 1.3 em
apart, short lines at body size alone on their line (`Total`, `Bill to:`),
and headings set larger than the body and not bold, one word or a line;
or, in `bold`, set at the body's size in the bold face. Nothing tells a
heading from the body but its size or its weight, which a scan gives OCR to
go by only in its pixels.

| set | what it is |
|---|---|
| `plain-1.2`, `plain-1.3` | headings a fifth and a third larger than the body, at 150, 200 and 300 dpi |
| `bold` | headings at the body's size in Helvetica-Bold, Times-Bold or Courier-Bold, at 150, 200 and 300 dpi |
| `underline`, `strike`, `boxed` | every line underlined; the prose struck through; a box drawn round each short line (200 dpi) |
| `superscript` | a raised `1` after every line, which OCR reads into it (200 dpi) |
| `italic` | the oblique and italic faces (200 dpi) |
| `blue-ink`, `red-ink`, `grey-ink` | `plain-1.2` at 200 dpi in coloured or faint ink |
| `inverted`, `white-on-blue` | light text on a dark ground |
| `yellow-paper`, `gradient`, `photo-bg` | a coloured paper, a ground that runs from white to grey, a soft photo-like ground |
| `noise`, `blur`, `jpeg`, `fax`, `skew` | sensor noise and specks, a 1.2 px blur, a quality-25 JPEG, a fax's threshold, the page turned 1.5 degrees |

The variants of `plain-1.2` are its images changed; their born-digital twin
is `plain-1.2`'s PDF. Each set's `meta.json` lists its pages' headings and
short lines.

## The check

A heading counts when a Markdown heading holds its text, normalised; a
short line counts against when one holds it. On `superscript` a `1` ending
a heading's text is the note mark and not part of it. The table gives, per
set and resolution, the headings OCR reads out of those the page sets, one
word and a line apart, and the short lines read as headings, each beside
what the born-digital page reads.

`--baseline` fails on a set whose images read fewer headings of either kind
than the baseline records, or more short lines as headings. It does not
hold the born-digital count: that is the general rules', which this set
does not test.
