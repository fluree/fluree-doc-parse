# fdoc convert

Convert documents to Markdown, XHTML, JSON, DoCO JSON-LD or plain text.

```bash
fdoc convert <FILE|DIR|->... [options]
```

Reads PDF, Markdown, plain text, HTML, DOCX, PPTX, XLSX, WebVTT and SubRip transcripts,
and email (`.eml`, `.msg`). PDF structure is inferred from layout; the others declare theirs
and carry no geometry. See [Input formats](../inputs/README.md).

```bash
fdoc convert report.pdf                       # Markdown to stdout
fdoc convert report.pdf -f doco -o out.jsonld
fdoc convert ./corpus/ --out-dir ./out -j 8
cat report.pdf | fdoc convert -
fdoc convert call.vtt -f doco                 # a transcript: one paragraph per turn
fdoc convert reply.eml --attachments ./att    # an email, its attachments saved
```

## Options

| Option | Description |
|--------|-------------|
| `-f`, `--format <FMT>` | `md` (default), `xhtml`, `json`, `doco`, `text`. See [Output formats](../formats/README.md) |
| `--md-links <MODE>` | how `-f md` writes links: `inline` (default) as `[words](address)`, `text` as the words alone. See [Links](../formats/markdown.md#links) |
| `-o`, `--output <FILE>` | write to a file; single input only. Conflicts with `--out-dir` |
| `--out-dir <DIR>` | write one output file per input into this directory |
| `--pages <RANGES>` | restrict to 1-based pages: `3`, `1-5`, `1,4,9-12` |
| `-j`, `--jobs <N>` | parallel workers for batch (`0` = one per core) |
| `--base-iri <IRI>` | what every node IRI minted by `-f doco` starts with (`<IRI>-element-3`). Default: `--doc-iri`, else `urn:fluree-doc-parse:<stem>-<first 12 hex digits of the SHA-256>`. One input only |
| `--doc-iri <IRI>` | stamp `-f doco` nodes with `doc:sourceDocument`, the tag a re-extraction retracts by. One input only |
| `--source-name <NAME>` | what `-f doco` records as the input's name, `doc:sourceName`. Default: the input's file name; none from stdin. One input only |
| `--attachments <DIR>` | save each email's attachments under `DIR/<email name>/`, to convert on their own. See [Email](../inputs/email.md#attachments) |
| `--layout-boxes <DIR>` | layout-detector sidecars. Env: `FDOC_TITLE_BOXES` |
| `--tier-results <DIR>` | model-tier readings to splice. Env: `FDOC_TIER_RESULTS` |
| `--structure-results <DIR>` | table-structure readings, for the [three-way veto](../concepts/escalation.md#the-three-way-veto) on a table reading from `--tier-results` or from a model called in the run; no table crop it vetoes is sent to the model. Env: `FDOC_STRUCTURE_RESULTS` |
| `--ocr-words <DIR>` | OCR word readings, read as text on the pages and regions the router sends a reader. See [OCR words](../integration/sidecar-formats.md#4-ocr-words----ocr-words). Env: `FDOC_OCR_WORDS`, which an explicit `--ocr` overrides |
| `--ocr <ENGINE>` | read those pages and regions with the built-in OCR engine, in this run: `oar-ocr`. Needs `--models`. Conflicts with `--ocr-words`. Env: `FDOC_OCR` |
| `--models <DIR>` | the OCR assets directory: the models and ONNX Runtime. Env: `FDOC_OCR_MODELS` |
| `--ocr-max-megapixels <MPX>` | most megapixels the OCR engine reads in one pass (default 12.5); a larger image is read in overlapping strips ([how an image is sized](ocr.md#how-an-image-is-sized)). Env: `FDOC_OCR_MAX_MEGAPIXELS`; config: `ocr.max_megapixels` |
| `--emit-anchors` | emit `[[VLM:…]]` tokens where escalated crops belong. With a reader (`--tier-results`, or a model called in the run), keep the ones it did not answer (a crop with no sidecar file, one `--pages` or `escalation.max_crops` left out), in every format, Markdown and text included; without it they are dropped. See [Anchors](../integration/anchors.md#interaction-with---tier-results). Env: `FDOC_VLM_ANCHORS` |
| `--probe-images` | decode the images to tell those holding text from pictures, without reading them: `doc:unreadRegions` then says `Region`, `Small` or `NotTexty` where a plain run says `Unprobed`. Escalation, OCR and the anchors probe already |
| `--escalate` | read escalated pages with the configured model in this run |
| `--no-escalate` | never call a model, whatever the config says |
| `--strict` | fail a PDF that yields nothing: no text, and no page or region reported unread. See [Exit codes](#exit-codes) |

The flags from `--layout-boxes` down wire the [escalation
tiers](../integration/escalation-tiers.md); without them you get tier 1.

## Escalation

With a provider configured, `convert` escalates the pages that ask for it and
splices the readings back — the whole loop, in this command. See
[`fdoc config`](config.md) for the setup and the on/off rules.

```bash
fdoc convert report.pdf -f doco          # escalates once a provider is set
fdoc convert report.pdf --no-escalate    # deterministic for this run
fdoc convert report.pdf --escalate       # force it; warns if nothing is set up
```

With nothing configured this never happens and no connection is opened.
`--tier-results` takes precedence: it supplies readings you already have, so
producing them again would be surprising.

`--pages` narrows what is *read*, not only what is printed, so inspecting one
page of a long document costs one crop rather than all of them.

## OCR

The pixels of a scanned page, or of a capture pasted into a text page, are
read by an OCR engine before analysis, and their words go through the same
layout pass as the text layer: paragraphs, tables and headings, each word with
its box. There are two ways to give `convert` those words.

**The built-in engine**, in one command:

```bash
fdoc convert scan.pdf --ocr oar-ocr --models ~/assets/ocr
```

The engine is PP-OCRv6 small through oar-ocr. Its models and ONNX Runtime are
not compiled into `fdoc`: they are read from the assets directory, each checked
against the hash `fdoc` was built with before it is loaded. Build that directory
with `tools/ocr-assets/build.py`. The engine is loaded once per run and shared by
every input and worker, when the first page or region is sent to it: a run
whose documents carry their own text never checks or loads it, and costs what
it costs without `--ocr`. An assets directory missing a file is refused at the
start. One whose files are not the ones this build loads is refused at the
first reading: the error is said once, a PDF's or DOCX's pages and pictures
are converted without OCR, an image file is not converted, and the run ends
with status 2. Name it in the [config](config.md#ocr)
and `--ocr` is no longer needed.

PP-OCRv6 small has no page orientation model, and the pipeline turns every
line at least 1.5 times as tall as wide a quarter counter-clockwise before
reading it. A sheet scanned upside down, or turned so its text runs up the
page, with no `/Rotate` to say so, reads as noise its recognizer is not
confident of. When the median confidence of a reading's level lines, or of
its tall ones, is under 0.8 (three lines at least, and only the lines
holding a letter when three of them do), the engine reads the image again
turned a half, where both read right. The reading more
confident by over 0.15 on average is kept. A line of three letters or more,
one of them a capital, one of `b d f h k l t` or a letter of another script,
is taken from the other reading when that one reads it more confidently by
that much, over the same place; a tall one, read turned a quarter in both,
when it reads it more confidently at all. Numbers and shorter words read as
something else turned a half, often confidently (`1068` as `8901` at 0.99):
they follow the lines that tell, taken from the other reading only where
most of the lines running their way were, so an upright page read poorly
keeps its numbers as read upright; a tall one, where no line running its
way tells (a table of numbers alone, set sideways), is taken as a tall line
that tells is. An
upright page holding a table whose text runs up keeps its text and takes
the table's. When two thirds of the
text, counted in characters, runs one way sideways and less than a quarter
of it is level, or two thirds of it runs upside down, or more of it upside
down than level with the two together a quarter of it (a sheet fed upside
down under the upright header a fax machine stamps, or carrying a table set
sideways), the page is read in
the frame where it runs upright, and every box is still placed on the page
as displayed. Words of one or two characters do not count: a digit is boxed
about as tall as wide and read at any angle. A page read upright, or with its text running down,
pays nothing; one read again costs a second reading (on synthetic letters,
forms and tables, 0.6 to 0.8 s more per page).

Limits: a page with upright text of its own, a quarter of its text or more,
keeps its frame, and the lines set sideways on it are laid along their
angle where they are, after the text above them, a table's cells not set
as a table. Their order is the order they lie in across the page, not the
order they read in: the lines of a block running down come out last first,
those of a block running up may come out shuffled, and a sideways table's
cells mixed with its header. A sheet fed sideways under an upright fax
header is such a page when the header is a quarter of its text (a sheet of
three lines or so): its lines come out last first, or shuffled. A page
whose text runs up and down in about equal parts, with no two thirds of it
either way, is not turned.
A page upside down with fewer than three level lines of two characters or
more is not read again. A number read the wrong way up reads as another
number, often confidently (`2024` at 0.92 where the words around it read
under 0.7), which is why only the lines holding a letter decide when there
are three of them: a page that is mostly a table of numbers is read again
on its text. With fewer, the numbers decide: a table of numbers with less
text than that, upside down or running up, is read again only if its
numbers read poorly, and otherwise comes out as wrong numbers with no
warning. Numbers written with the digits 0, 1, 6, 8 and 9 alone read as
other numbers turned a half with a confidence of about 1 (806 as 908): a
table of such numbers alone set sideways, with no line of words running its
way, may come out as other numbers. A page is read again turned on the median
confidence of its tall lines: text running up next to more tall lines of
words read well (a block running down beside it) is not read again and
comes out as noise, and neither is a single line running up (a label set
up a form's margin). A word of a character or two set sideways may be
misread. A page turned a quarter reads fewer of its headings: the detector
boxes a long line of sideways text a few pixels thicker than the same
line upright, and a short heading's box no thicker, so a heading comes out
nearer the size of its text and is read as text, and a line of text can
come out larger than it and read as a heading.
Only the built-in engine reads a page again turned: [`fdoc dev render-routed`](dev.md) writes the
crops as the page shows them, an outside engine or a model (`--escalate`)
reads them unturned, and the manifest says nothing of the page's
orientation. Words an outside engine wrote with their `angle` are laid as
[sidecar formats](../integration/sidecar-formats.md) describes.

**Any other engine**, through files: render the crops, read them with your
engine (or with [`fdoc ocr`](ocr.md)), then convert with the words:

```bash
fdoc dev render-routed scan.pdf crops/ --ocr-regions --stored-resolution
your-ocr crops/ --out words/          # or: fdoc ocr crops/ --models ~/assets/ocr --out words/
fdoc convert scan.pdf --ocr-words words/
```

Each crop is read at the resolution of the image under it, between 2× and
300 dpi (`--stored-resolution` on `render-routed`; see [producing the
crops](../integration/escalation-tiers.md#producing-the-crops)), where a model
gets 2×.

A bare PNG, JPEG or TIFF is read the same way, as one page decoded upright;
see [Images](../inputs/images.md). So are the pictures in a DOCX, whose text
follows their figure (see [Office files](../inputs/office-and-web.md)), the
pictures anchored to an XLSX sheet's cells, placed by their anchor, and
the images an e-mail shows in its body, whose text is placed where they are
shown (see [E-mail](../inputs/email.md#images-in-the-body)). A
picture nothing read is counted on stderr and listed in the graph's
[`doc:unreadImages`](../formats/doco.md#images-nothing-read).

What either reads is the same: the pages whose text is pixels (scanned,
near-blank, broken text), whole, and on other pages the raster regions the
text layer does not cover. A model is only asked for regions over a tenth of
the page whose pixels the probe takes for text; an OCR engine reads the
small ones too, and nothing reads one the probe does not take for text (a
photograph, a chart). Born-digital text is never
replaced. With a model reader configured as well, it reads what OCR does not:
pages in doubt about their structure, and tables. It also reads what OCR read
but cannot give whole, in place of OCR's reading:
- a page whose text is all pixels;
- a reading the engine is unsure of (under 0.90 confidence, averaged over 20
  characters or more);
- pictures that hold as much text as the page's own layer;
- a table in a picture (three rows or more of three boxes or more).

The small regions only an engine reads stay its own. `-v` names each crop left
to the model and why.

A picture's reading that the engine is unsure of (under 0.90), on a page with
text of its own, is set aside, model or no model. It is lettering drawn into
artwork, a logo's or a seal's texture, a photo's grain read as words, and laid
on the page it would be text the page never printed. A whole page's reading,
or the regions of a page with no text layer, are kept however unsure: they are
all the text there is.

On a page with text of its own, each picture's reading is read apart from the
page's text. Images that touch or overlap make one picture, and a word whose
last letters run past a picture's edge stays with that picture.
- **Lettering** (a chart's labels, a diagram's names) is the figure's text. It
  no longer breaks the page's columns, and is no longer gridded into a table
  with the prose beside it.
- **Text set as an image** reads as paragraphs where the picture stands. Such
  a picture has most of its lines running across it, or text covering a fifth
  of it or more. Its paragraphs follow the text over the picture in its lane,
  and any table over it there. A picture at the top of a page, over all its
  text, comes first, as a page's logo does.
- **The page's own text, set as an image**, stays the page's text. That is a
  letter drawn as an image of its own, or a word set as an image in a line of
  the page's text, such as a wordmark among a chart's labels.

The text read in a picture does not count toward a page's doubt about its
figures, which can send the page to a model: it lies inside its picture
whatever the page's layout.

`fdoc` built without the `ocr` feature has no engine and still reads
`--ocr-words`.

## Inputs

Files, directories, or `-` for stdin. **Stdin is read as a PDF, an email, a
transcript or an image** (PNG, JPEG, TIFF), which are told apart by their
content; Markdown, HTML and the Office formats are known only by their
extension, so they are read from a file.

Directories are scanned for every extension `convert` reads, including
Word, PowerPoint and Excel's macro-enabled, template and show variants
(`.docm`, `.dotx`, `.potx`, `.ppsx`, `.xltx`, …). Multiple inputs are
allowed; with more than one you need `--out-dir` rather than `--output`.

## Output naming

With `--out-dir`, each output is the input stem plus the format's extension
(`.md`, `.xhtml`, `.json`, `.jsonld`, `.txt`). No two inputs write one file:
where inputs share a stem, the source extension joins it (`report.pdf.md`,
`report.docx.md`), and where that still repeats — `a/report.md` and
`b/report.md` — later ones are numbered (`report.md (2).md`). Names are
compared without case, as a case-insensitive file system would.
`--attachments` names each email's directory the same way.

## Batch and parallelism

```bash
fdoc convert ./corpus/ --out-dir ./out -j 8
fdoc convert ./corpus/ --out-dir ./out -j 0    # one worker per core
```

A document that fails does not stop the batch: the rest are written and the
exit code is non-zero. Use `-v` to see which failed and how long each took.

## Page ranges

```bash
fdoc convert report.pdf --pages 1-5
fdoc convert report.pdf --pages 1,4,9-12
```

Ranges are **1-based** — matching what a PDF viewer shows — while the `page`
field in the output is **0-based**. The whole document is still parsed and
analyzed; the filter applies to the emitted elements, so cross-page structure
(sections spanning a boundary) is resolved before the cut.

## Exit codes

`0` on success, non-zero if any input failed.

A PDF can open without error and still give nothing: a page tree that
loops, one nested deeper than the parser follows, a page reference broken
by one flipped bit. Each reads as a valid document with no pages, exactly
like a blank one. Every page or region the router can tell carries content
is [reported unread](../formats/doco.md#pages-nothing-read) when nothing
reads it, so an output that holds no element *and* reports nothing unread
is warned about:

```
warning: report: nothing was read: no page was found (the file may be damaged)
```

With `--strict` that input fails instead: `error:` on stderr, no output
written, and a non-zero exit (in a batch, it counts among the failed
inputs). A page left out by `--pages` was not asked for and does not count,
but a `--pages` that keeps none of the document's pages reads nothing, and
is warned about (or fails) the same way.
A page with no text layer and no image warns too, whatever it is: blank,
its text drawn as vector outlines (the router does not count drawing
operations, so such a page is never reported unread), or damaged; nothing
in the output tells them apart. Only PDFs are checked this way.

## Compatibility forms

`fdoc md`, `fdoc json` and `fdoc xhtml` are hidden single-file equivalents of
`convert <file> --format <fmt> --no-escalate`, kept for scripts that shell
them. Prefer `convert`.
