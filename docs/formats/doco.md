# DoCO JSON-LD

```bash
fdoc convert report.pdf -f doco
fdoc convert report.pdf -f doco --base-iri https://example.org/docs/report
fdoc convert report.pdf -f doco --doc-iri https://example.org/docs/report
```

The richest output: a JSON-LD graph with explicit section containment, table
cells as addressable nodes, character offsets, and page/bbox provenance. It is
insertable into a [Fluree](https://flur.ee) ledger as-is.

## The context

```json
{
  "@context": {
    "doc":      "https://ns.flur.ee/doc#",
    "doco":     "http://purl.org/spar/doco/",
    "nif":      "http://persistence.uni-leipzig.org/nlp2rdf/ontologies/nif-core#",
    "po":       "http://www.essepuntato.it/2008/12/pattern#",
    "po:contains": { "@type": "@id" },
    "po:containsAsHeader": { "@type": "@id" },
    "rdfs":     "http://www.w3.org/2000/01/rdf-schema#",
    "dcterms":  "http://purl.org/dc/terms/",
    "foaf":     "http://xmlns.com/foaf/0.1/",
    "xsd":      "http://www.w3.org/2001/XMLSchema#"
  },
  "@graph": [ … ]
}
```

One Fluree namespace, `doc:`, for what no standard ontology defines, plus
three public ontologies, `rdfs` for the display label, Dublin Core
(`dcterms`) for what a document declares about itself, with `xsd` typing its
dates, and FOAF for the root's class, `foaf:Document`. `doco` is the Document Components Ontology; `po` is the Pattern
ontology DoCO extends, and `po:contains` is the containment property DoCO
itself specifies; `nif` is the NLP Interchange Format, whose character offsets
are the join point with annotation and NER tooling. Everything
Fluree-specific — placement, evidence, table cells, structure hints — lives
under `doc:`. See the full [vocabulary](../reference/vocabulary.md).

`po:contains` is IRI-coerced, so containment edges are real references rather
than strings — which is what makes the graph traversable after insertion.

## An element

```json
{
  "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-2",
  "@type": "doco:Paragraph",
  "doc:bbox": "91.17,185.64,145.00,195.09",
  "doc:evidence": "layout",
  "doc:pageIndex": 0,
  "doc:xhtmlTag": "p",
  "nif:beginIndex": 0,
  "nif:endIndex": 5,
  "nif:isString": "小田切 亘",
  "rdfs:label": "小田切 亘"
}
```

| term | what it carries |
|---|---|
| `nif:isString` | the full element text |
| `rdfs:label` | a display preview, capped at 100 chars |
| `nif:beginIndex` / `nif:endIndex` | char offsets into [`-f text`](text.md) |
| `doc:pageIndex` | 0-based physical page — [not the printed number](../reference/vocabulary.md#why-pageindex-and-not-pagenumber) |
| `doc:bbox` | `"x0,y0,x1,y1"`, PDF units, top-left origin |
| `doc:evidence` | [which signal classified it](../concepts/provenance.md) |
| `doc:xhtmlTag` | the equivalent HTML tag |
| `po:contains` | children, for `foaf:Document`, `BodyMatter`, `Section`, `Table` |
| `doc:sectionLevel` | heading depth, on `doco:Section` |

`doc:bbox` is absent for sources without geometry — see [Measured vs
declared structure](../concepts/geometry-vs-declared.md).

## The document node says what was read

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-0",
  "@type": "foaf:Document",
  "doc:sha256": "661511bb2b30c4e8a9f2d71b05e3c6a48f90d2b17e5a3c8f4b6d1e09a7c25f3e",
  "doc:sourceName": "report.pdf" }
```

`doc:sha256` is the input's bytes as lowercase hex SHA-256, always present.
The same file found twice — two copies on a drive, one file under two names —
has one value, so duplicates are a query rather than a guess. It identifies
bytes, not text: a PDF exported again, or a document saved again, is a new
hash even where nothing it says has changed.

`doc:sourceName` is what the caller calls the input: the file name `fdoc
convert` was given, or `--source-name`. Standard input has none. A name is
not an identifier — names repeat, and change when a file is moved — which is
why the hash is the one to join on.

What the file declares about itself rides beside them, in Dublin Core:

```json
{ "@type": "foaf:Document",
  "dcterms:title": "Quarterly Report",
  "dcterms:creator": ["Ada Park"],
  "dcterms:created": { "@value": "2019-07-12T15:10:45-06:00", "@type": "xsd:dateTime" },
  "dcterms:modified": { "@value": "2019-07-12T15:11:13-06:00", "@type": "xsd:dateTime" } }
```

From a PDF's information dictionary, an Office file's core properties, an
HTML page's `<title>` and author, or an email's headers. Declared, never
inferred: a title guessed from a page's largest line would be a reading of
the page, and the [elements](#sections-are-explicit) are where that goes. A
date that is not a real one is left out, since typed as `xsd:dateTime` it
would fail a store's insert.

## The document node carries page geometry

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-0",
  "@type": "foaf:Document",
  "doc:pages": { "@type": "@json",
                 "@value": [ { "pageIndex": 0, "width": 612.0, "height": 792.0 },
                             { "pageIndex": 1, "width": 612.0, "height": 792.0,
                               "folio": "2" } ] } }
```

A `doc:bbox` cannot be placed on a rendered page without these: the consumer
needs the ratio between the page's own units and the pixels it rendered to,
and this is the only place that denominator appears. Sources with no geometry
— Markdown, DOCX — omit the key rather than reporting a zeroed size.

The size is the page *as displayed*. A page that displays sideways — a
`/Rotate 90` page, a landscape table bound into a portrait document — is read
in the frame where its text is upright, and its elements' boxes are mapped
back to the displayed frame, so a box and a page render always agree.

`folio` is the page number printed on the page, where the document numbers
its pages: the one piece of [running furniture](../design/furniture.md) that
identifies a page rather than the document. A string, because front matter is
`iv`. Absent on pages that carry no page number.

## What the pages say about the document

```json
{ "@type": "foaf:Document",
  "doc:runningText": { "@type": "@json",
                       "@value": [ "CHURCH &", "DWIGHT",
                                   "TM004361 Rev:005 Production" ] } }
```

The header and footer text [furniture detection](../design/furniture.md)
removed from the body, kept once here. The header a PDF prints on its first
page and the footer on its last are also kept once in the text, as elements
marked `header` and `footer`. A Word file states its header and footer, so
they are read into the text the same way, and their lines are kept here as
well: a document's identity is in one place whatever it was read from.

Repetition is what makes a line noise *inside* the body and exactly what makes
it identify the document. A controlled procedure puts its owner, title and
number in a block on every page — on a three-page one, all three — so removing
the repetition removes the identity, and the document comes out anonymous.

Every running line is kept here, including those the text leaves out: a
running head that starts partway through the document has no place in the
text that would not split two pages' content. A running head that names the
section the page is in is listed here too, and is kept in the text once,
where it is first printed, at the start of its section. Page numbers are excluded, bare
or as `Page 3 of 15`: a folio identifies nothing about the document. It
identifies a page, and is kept on that page's entry in `doc:pages`. A line
the same on every page is no page number, and is kept however it reads: a
telephone number, a revision year. The lines are in the order they are
first printed: page by page, and down each page as it reads.

## Pages nothing read

```json
{ "@type": "foaf:Document",
  "doc:unreadPages": { "@type": "@json",
                       "@value": [ { "pageIndex": 0, "reason": "Scanned" } ] } }
```

Present only when a page carries content the output does not hold — a scan, a
vector drawing, glyphs whose Unicode cannot be trusted — and no reader
supplied it. Without this an empty page and an unread page look identical;
one document of engineering drawings produced 126 bytes of XHTML with nothing
saying 99% of it was missing.

`reason` is the router's verdict: `Scanned` (an image covering half the page
or more, and next to no text layer), `NearBlank` (next to no glyphs and a
smaller image: a logo on a separator sheet, a signature) or `BrokenText`;
or `OcrSparse`, a PDF's page an OCR reading (`--ocr-words`) read whole that
still holds next to no text — an engine that gave a few words for a scan it
could not read. An image given on its own is read once OCR took a word in
it, however few: its words may be all it holds. Escalating the page clears
the entry, however short the model's reading, because then something did
read it. A reading that came back with
no text, by OCR or by a model, does not: the router took the page for
content, and nothing in the output stands for it. A placed picture is held
to another rule ([below](#images-nothing-read)).

Or it is `LaterPage`: a page after the first of a multi-page image file (a
TIFF), which nothing reads. Its `pageIndex` is the page's place in the input
file, 0-based like every other, and by design it has **no entry in
`doc:pages`**: that list holds the pages read, and this one was not, so
nothing was measured to put there. A consumer joining `doc:unreadPages` to
`doc:pages` on `pageIndex` finds no match for it, and should not expect
one. A TIFF of many pages lists each; the warning `fdoc` prints gives them
as a range (`pages 2–10000`).

Or it is `PastPageLimit`: a page of a PDF or an XPS document past the most a
document is read to (10,000, `--max-pages`), listed one entry a page, with
no entry in `doc:pages` either; of a TIFF, the first page past it, after
which its pages are not counted. Or `PastMemoryLimit` or `PastWorkLimit`:
a page of a PDF on or after the one its memory or its work ran out on,
before the page limit, the document read to the page before
(`--max-memory`, `--max-work`). A consumer asking whether a document was
read whole looks for any of these three
([Limits](../reference/limits.md#pages)).

A consumer that validates `reason` against a fixed set should accept every
one above.

Deliberately not an element. A marker element would have to carry text to be
visible, and inventing text puts characters into the projection that every
`nif:beginIndex` in the graph is counted against.

## Regions nothing read

```json
{ "@type": "foaf:Document",
  "doc:unreadRegions": { "@type": "@json",
                         "@value": [ { "pageIndex": 2,
                                       "bbox": "56.00,582.00,556.00,732.00",
                                       "reason": "Region" } ] } }
```

The same, for part of a page: a screenshot or a scanned table pasted into a
page of prose. Such a page holds plenty of text, so it is never in
`doc:unreadPages`, and the region's content would be missing with nothing
saying so. Present when a raster region the text layer does not cover was
not read: no OCR words taken for it, no model reading of it (`r{i}`) or of
its whole page (`full`) spliced in. An image the page draws twice in one
place is one region, listed once.

`bbox` is written as [`doc:bbox`](#an-element) is: PDF units, top-left
origin, the page as displayed, so it joins `doc:pages` the same way, and a
consumer can crop the region from a rendered page and read it. `reason` says
what the router made of it:

| reason | the region | read by | warned about |
|---|---|---|---|
| `Region` | its pixels look like text or a table, and it covers a tenth of the page or more | a model (`r{i}`), an OCR engine | yes |
| `Small` | its pixels look like text, but it covers under a tenth of the page; half an inch (36 units) or more on its longer side | an OCR engine only (`small_region`) | no |
| `NotTexty` | a tenth of the page or more, but the pixel probe does not take it for text: a photograph, a chart, an illustration | nothing | no |
| `Unprobed` | not examined: a plain run decodes no image, and a run without OCR none under a tenth of the page, so nothing tells a capture of text from a photograph; half an inch or more on its longer side, at any share of the page | nothing in this run | no |

The images are probed only by a run that acts on the verdict: escalation,
OCR (`--ocr`, `--ocr-words`), the anchors or the layout sidecars, or
`--probe-images`. The decode is most of what an image costs, so a plain run
skips it and lists each image the text layer does not cover as `Unprobed`;
a run that probes lists the same images as `Region`, `Small` or `NotTexty`.
A run that probes and reads no image (escalation without OCR) leaves the
images under a tenth of the page unexamined in the same way. `Unprobed` is
listed here only: a plain run reads no image by design, so it warns about
none, and the Markdown and XHTML carry no comment for them.

The router sends a model only to `Region`; it declines `Small` and
`NotTexty`. An
OCR engine (`--ocr`, `--ocr-words`) reads the `Small` ones, and one it read
is not listed: it runs locally and bills nothing, where a model call is
billed and sends the image off the machine. A `NotTexty` region is read by
neither: the probe is what tells where text is, and what an engine reads in
a photograph is noise. The probe takes a screen capture for text when its
pixels are mostly dark or light ink on paper, or when they hold lines of
text on a ground of one colour, a few lines at the top of a wide, light
screen among them.

What an engine reads in any region (`r{i}`, routed or small) is held to a
noise rule all the same, a net behind the probe for a photograph or a
gradient it took for text: the reading is set aside when its words' mean
confidence is under 0.5; or, in a small region only, it is under 0.8 and
more than two thirds of them are fragments of one or two letters or
figures (`1`, `I`, `J`), as an engine reads foliage, whose noise it may
score as surely as a date stamped in a landscape; or one word is boxed
over nine tenths of the region or more and is a fragment or read under
0.8. A routed region is not judged by its fragments: a raster table of
one- and two-digit cells is mostly fragments, and is kept however surely
it is read over 0.5. A crop of one line of text, or a
sign whose one word fills it, read surely, is kept. A reading scored on no
scale, or with a score outside 0-1, is judged by its boxes alone. The
region then stays listed. A reading of the whole page (`full`) is not
judged: a scan's lines are words. Short lines of CJK signage (`出口`) and
the short tokens of a reading boxed word by word count as fragments: read
under 0.8 in the mean, such text can be set aside.

Unread, the declined regions are listed anyway so a caller can decide: crop
and read them, or take them for the logo, photograph or ornament they are as
often as not. For the same reason only `Region` is in the stderr warning and
the Markdown and XHTML comment, so a letterhead or a photograph, examined or
not, never warns.
An image under a tenth of the page that does not look like text either (an
icon, a bullet, a small photograph) is not listed at all, nor any image the
page's own text covers. Nor is a texty one under half an inch on its longer
side: a checkbox or an icon, which a stencil (a 1-bit image mask) draws and
the probe takes for text whatever it holds, so a form would list every box.
An OCR engine is still offered those whose own pixels hold a line of text;
a one-line capture of a date is a couple of inches wide and is listed. No
image under 10 pixels on its short side is listed or read, whatever size it
is drawn at: a rule stretched from a 1 × 1 mask holds no line of text.

## What the parser reported

```json
{ "@type": "foaf:Document",
  "doc:parseWarnings": { "@type": "@json",
                         "@value": [ { "pageIndex": 3,
                                       "kind": "ImageDecodeFailure",
                                       "count": 1 } ] } }
```

What the PDF interpreter reported while reading a page, once per page and
kind, with how many times. `ImageDecodeFailure` is an image it could not
decode: it is placed all the same, so without this the page reads exactly
like one whose image decoded and held nothing to read. `UnsupportedFont`, a
font it cannot read, is passed on too, though the interpreter this version
uses never reports it. An image several pages share is decoded each time a
page draws it, so it is reported on every page that draws it. An image mask
(a stencil, the 1-bit form some scans arrive in) is decoded while a page is
read only when it is small, 512 × 512 pixels at most, to tell a checkbox
drawn again from another image: a larger one that is broken is not reported.

Each kind is also one warning on stderr, naming its pages (the first ten):

```
warning: report: ImageDecodeFailure on page 4: an image could not be decoded; what it shows is missing
```

Over the PDFs in this repository's test and evaluation fixtures, none is
reported: a clean document says nothing.

## Images nothing read

```json
{ "@type": "foaf:Document",
  "doc:unreadImages": { "@type": "@json",
                        "@value": [ { "figure": "figure-0-1", "image": "m1",
                                      "part": "word/media/image1.png",
                                      "reason": "NotRead" } ] } }
```

The same statement for the images a document places in its flow: a picture
in a DOCX, a picture anchored to an XLSX sheet's cells, an image an e-mail
shows in its body. Their text is read as a bare image's is — by OCR
(`--ocr`, `--ocr-words`), by a model's reading supplied (`--tier-results`)
or by the configured model — and placed after the figure the image is drawn
as. An image nothing read is listed here, so a
pasted screenshot whose text is missing is not taken for a picture with
none.

`figure` joins to the figure's `doc:figure`; `image` is the name its crop
and its words go by (`<stem>_m1.png`, `<stem>_m1.json`); `part` is where the
file keeps it: a package part, or for an e-mail the `cid:` URL its body
names it by, or `attachment <n>` (its place in `doc:attachments`, from 1)
where no Content-ID names that part alone, never its file name, which two
images can share. An image several figures draw (a logo
or a screenshot pasted more than once) is read once, under the name of the
first figure drawing it, and listed unread once for each figure. `reason` is
one of eight:

| `reason` | the image | counted on stderr |
|---|---|---|
| `NotRead` | nothing was asked to read it, no reading of it was supplied, or the configured model was not sent it, past `escalation.max_crops` | yes |
| `Missing` | the file does not hold it: a link to an outside file, a relationship the file does not define | yes |
| `UnsupportedFormat` | not a PNG, JPEG or TIFF: a vector drawing kept as EMF or WMF, a GIF | no |
| `PastAllowance` | past what one document may have read: 256 images, 512 million pixels as their headers state them (an image whose header does not parse is charged a pixel per byte) | yes |
| `PastMemoryLimit` | read, but what its reading makes (its elements, their text, a table's cells and the copies DoCO writes of them) is past what the document, and the images read before it, left of the memory one document may take (`--max-memory`); the configured model is not sent it ([limits](../reference/limits.md#copies-of-a-tables-cells)) | yes |
| `Small` | nothing else read it, and the configured model is not sent it: under 400 pixels on its longer side or 48 on its shorter | no |
| `NotTexty` | nothing else read it, and the configured model is not sent it, as the pixel probe does not take it for text (a photograph, a chart, a logo); or OCR read it, and what it read was noise, as a photograph or a gradient read for text gives: the rule a [`NotTexty` region](#regions-nothing-read)'s reading is held to, and the reading is listed with `setAside: "noise"` ([OCR readings](#ocr-readings)) | no |
| `Failed` | its reading failed, or was set aside because its words could not be placed on it (told before the reading is judged noise), the part it names is not there or cannot be read, its part is empty or ends before its format can be told, or its header does not parse, so the configured model is not sent it; stderr says why | yes |

An image read that held no text was read, and is not listed. The pixel
probe gates the model's call alone, which is billed and sends the image
off the machine: an OCR engine (`--ocr`) reads a placed picture whatever
the probe makes of it, and a reading supplied for it (`--ocr-words`,
`--tier-results`) is taken whatever it shows.

The list is written whatever reads the images, with no reader at all: a
pipeline keeping the graph learns which pictures hold text it does not have.

Unlike an unread page, it adds no comment to Markdown or XHTML: most
documents carry a logo or a photo with no text in it, and a warning on each
would be noise. The note on stderr counts them, except those the table marks
as not counted: no engine decodes them (`UnsupportedFormat`, told by the
image's first bytes whatever is configured, and never warned about), they
are under the model's floor (`Small`, which `--verbose` names, with `--ocr`
to read them) or they are not taken for text (`NotTexty`): nearly every
signed e-mail carries a logo, a GIF or a small PNG, a vector drawing has no
text to read anyway, and a photograph is what it is as often as not, so
these are listed here only, as a PDF's `Small` and `NotTexty` regions are.

## OCR readings

```json
{ "@type": "foaf:Document",
  "doc:ocrReadings": { "@type": "@json",
                       "@value": [
    { "pageIndex": 0, "crop": "full", "engine": "pp-ocrv6-small/oar-ocr", "words": 212,
      "meanConfidence": 0.9843, "minConfidence": 0.3121 },
    { "pageIndex": 2, "crop": "r1", "words": 9 },
    { "image": "m1", "crop": "full", "words": 2, "meanConfidence": 0.28,
      "minConfidence": 0.1, "setAside": "noise" } ] } }
```

Present only when an OCR reading (`--ocr`, `--ocr-words`) was taken into the
output: one entry per reading, in page then crop order. `crop` is the crop
read, `full` or the region `r{i}`, as the crop files are named: the routed
regions first, numbered as a model's crops are, then the small ones no model
is sent (`Small`), which only an OCR engine reads. A picture placed in a
document's flow (a DOCX's, an XLSX sheet's, an image an e-mail shows in its
body) has no `pageIndex`; it goes by `image`, the name its words go by (`m1`,
`m3_1`). `engine` is the engine the reading names, `words` the words taken,
or the lines for an engine that boxes lines rather than words (the built-in
one does), so the count is not comparable across engines. `meanConfidence`
and `minConfidence` are over the words (or lines) taken, to four decimals,
and absent when the engine gave no score. One score outside 0-1 (Tesseract's
0-100, not divided) leaves the whole reading without them, with a warning on
stderr: its other scores are on that scale too, and could not be told apart
from 0-1 ones. A reading whose words are not in the output is not listed,
but for one set aside as noise (`setAside`, below): one
refused (not routed to a reader, superseded by the page's whole reading,
unplaceable: all of its words land off what it read (the page, or its region
with the crop's margin) or have no usable box, or most of them do in a
region's reading or in a whole reading of a page whose text is all pixels),
and one a model's reading replaced (a page or a picture read whole through
`--tier-results` or the configured model). One that took no word is listed, with
`words: 0`, and one set aside as noise with what it held, unless a model's reading replaced its page or read every region
the reading was to read: its region, for a region's reading; every region an
OCR engine reads on its page, for a whole reading of a page of text. Those
include the small regions no model is sent (`Small`), so a whole reading of a
page carrying one stays listed: it is the only reading of that region.

The confidence is the engine's own score, reported as information. The
built-in engine's (PP-OCRv6 small) is packed near 1: the measurements in
[issue #4](https://github.com/fluree/fluree-doc-parse/issues/4) find it much
the same whether that engine read well or poorly. So it is useful to spot a
reading that failed as a whole — a low mean, a page that came back as noise —
not as a guarantee about any one fact: a single misread digit barely moves
it. Another engine's score, given through `--ocr-words`, may tell good
readings from bad better (Tesseract's did in those measurements). Whatever
the engine, nothing is routed on it, and the one rule applied is a net
behind the pixel probe, for a photograph or a gradient it took for text: a
reading of a region (`r{i}`) or of a picture whose mean is under 0.5, or
under 0.8 with most of its words fragments (a small region's or a
picture's: a routed region is not judged by its fragments), or with one
fragment, or one
word read under 0.8, boxed over nearly all of it, is noise, set aside before
it is taken ([regions](#regions-nothing-read), [images](#images-nothing-read)),
and listed here with `setAside: "noise"`: `words`, `meanConfidence` and
`minConfidence` are then those of the words it held, none of which is in the
output, so the confidence that made it noise shows. A reading of a whole page (`full`) or of an
image read as a file is not judged: a scan's lines are words. A consumer that wants a second reading where OCR
may be wrong is better served by comparing two engines than by a cut on this
number.

## Sections are explicit

The flat element list becomes a tree here. A `doco:Section` node is minted per
heading, carrying `doc:sectionLevel` and containing the title plus everything
under it. The title is the section's header, held by `po:containsAsHeader` as
DoCO specifies, and by `po:contains` with the rest, so everything a section
holds is one hop:

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-section-2",
  "@type": "doco:Section",
  "doc:sectionLevel": 1,
  "po:containsAsHeader": "…-element-3",
  "po:contains": [ "…-element-3", "…-element-4", "…-table-5" ] }
```

A document's own title is the document node's header in the same way: see
[the title](#the-title-its-subtitle-and-its-kickers).

`po:contains` is a set, as JSON-LD arrays are. Reading order is
`nif:beginIndex`, which every node holding text carries; DoCO's own
suggestion, a linked list of items, would be a node per item in every
container.

Every node of a document shares one counter in emission order, so IRIs are
`{base}-section-{n}`, `{base}-element-{n}`, `{base}-table-{n}` and so on, with
`n` never reused. See [IRIs and re-extraction](#iris-and-re-extraction) for
the base.

## The title, its subtitle and its kickers

A document's own title is a `doco:Title`, held as the document node's
header by `po:containsAsHeader`, as a section's heading is held by its
section. Its kicker and subtitle are in the body, on either side of it in
reading order. From HSE's `Health and safety at quarries`:

```json
{ "@id": "…-element-0", "@type": "foaf:Document",
  "po:containsAsHeader": "…-element-3",
  "po:contains": [ "…-element-1", "…-element-3" ] }
{ "@id": "…-element-1", "@type": "doco:BodyMatter",
  "po:contains": [ "…-element-2", "…-element-4", "…-section-5", … ] }

{ "@id": "…-element-2", "@type": "doco:Paragraph", "doc:evidence": "kicker",
  "nif:beginIndex": 0, "nif:isString": "Health and Safety Executive" }
{ "@id": "…-element-3", "@type": "doco:Title", "doc:evidence": "metadata",
  "nif:beginIndex": 29, "nif:isString": "Health and safety at quarries" }
{ "@id": "…-element-4", "@type": "doco:Subtitle", "doc:evidence": "subtitle",
  "nif:beginIndex": 60, "nif:isString": "Quarries Regulations 1999" }
```

**Finding the title.** Follow the document node's `po:containsAsHeader`. A
document has one title at most: a second `doco:Title` in the element list is
emitted as a level-1 heading. The title opens no section. What follows it up
to the first heading is the body's, and the sections start at level 1.

**Where it comes from.** Its `doc:evidence` says how it was decided:

| source | the title | `doc:evidence` |
|---|---|---|
| Word | the paragraph in the Title style | `docx` |
| PowerPoint | the title on the deck's title slide | `pptx` |
| PDF | the first page's title, where it is sure | `metadata`, `outline` or `type` |

A PDF states no title of its own in its text, so its title is read off the
first page, and is a `doco:Title` only where it is sure:

- `metadata`: the file's own title (`/Title`) names the line.
- `outline`: a bookmark at the outline's first depth names it.
- `type`: its type sets it apart as the page's title, and nothing doubts it.

**No title.** Where no title is sure, the document node has no
`po:containsAsHeader`, and the graph has no `doco:Title`: the document is
untitled. In a PDF, a line in doubt is a level-1 `doco:SectionTitle`: a date, a
section's name or number, a division's label, a company's name, or a journal's
mark. No title is better than a wrong one. The document node's
[`dcterms:title`](#the-document-node-says-what-was-read) is what the file
declares, whether there is a `doco:Title` or not. It may differ from the
`doco:Title`, or be absent.

**The subtitle.** An explanatory or alternative title set under the title is
a `doco:Subtitle`, the element directly after the `doco:Title` in reading
order (`nif:beginIndex`). It is in the body matter, as text is, not held as a
header, and it opens no section. A document has one at most. To read it, take the element after the
title when it is a `doco:Subtitle`.

| source | the subtitle | `doc:evidence` |
|---|---|---|
| Word | the paragraph in the Subtitle style | `docx` |
| PowerPoint | the first paragraph of the title slide's subtitle | `pptx` |
| PDF | a line set as the title's subtitle | `subtitle` |

In a PDF, a subtitle is read only under a title that is sure. It is the line
directly after the title, on its page, that:

- is set close under the title, a step smaller than it and larger than the
  body, in one or two lines;
- has no number, and is not a section's or a division's name;
- is set unlike every other heading of the document. A line set as the
  sections are opens one.

**Kickers.** A PDF's kicker is a `doco:Paragraph` before the title with
`doc:evidence` `kicker`. It is a category, series name or tagline set over
the title, smaller than it, with nothing between (`Scalable. Responsive.
Innovative.` over `Exos X16`). It is text, not a heading. Over a line in doubt,
which is a heading and not a title, the same line is a plain paragraph.

**Running banners.** A line repeated at the head or foot of the pages
(`Contains Nonbinding Recommendations`, `UNCONTROLLED COPY WHEN PRINTED`) is
not in the body. It is listed once on the document node, in
[`doc:runningText`](#what-the-pages-say-about-the-document). On a one-page
document nothing repeats, so the banner is a paragraph.

**In the other formats.** [Markdown](markdown.md) writes the title as `#`,
and the subtitle and kickers as plain lines. [XHTML](xhtml.md) writes the
title as `<h1>`, and the subtitle and kickers as `<p>`.
[JSON](json.md) carries the same `type` and `evidence` on each element.

## Lists contain their items

A run of list items is a `doco:List` containing one `doco:Paragraph` per
item, with `doc:xhtmlTag` `li`. DoCO has no list item class: a list's members
are typed as what they are, and being contained by the list is what makes
them members. Their order is their `nif:beginIndex`.

A drawing's text read between two items ends the list, unless the items
count on across it, `3.` then `4.`. A numbered list run on into the next
column, with a photo's callouts read between the columns, stays one list,
and the callouts stay figure text where they fall.

```json
{ "@type": "doco:List", "po:contains": [ "…-element-7", "…-element-8" ] }
{ "@id": "…-element-7", "@type": "doco:Paragraph", "doc:xhtmlTag": "li",
  "nif:isString": "Sign the order" }
```

A list nested in an item is contained by that item: the item's
`doco:Paragraph` contains a `doco:List` of its own, after its text. Items
further down that list hold lists of their own the same way, as deep as the
source nests them.

```json
{ "@type": "doco:List", "po:contains": [ "…-element-7", "…-element-10" ] }
{ "@id": "…-element-7", "@type": "doco:Paragraph", "doc:xhtmlTag": "li",
  "nif:isString": "Before you start:", "po:contains": [ "…-element-8" ] }
{ "@id": "…-element-8", "@type": "doco:List", "po:contains": [ "…-element-9" ] }
{ "@id": "…-element-9", "@type": "doco:Paragraph", "doc:xhtmlTag": "li",
  "nif:isString": "Turn off the power" }
{ "@id": "…-element-10", "@type": "doco:Paragraph", "doc:xhtmlTag": "li",
  "nif:isString": "Mount the bracket" }
```

Each item's text is its own, so the parent's `nif:isString` and offsets
don't take in the items nested under it.

The element model marks an item `doc:ListItem`, with its `depth`, and
[`-f json`](json.md) shows both, because a flat list of elements has no list
to be contained by.

## Table cells are nodes

Each cell is addressable, with its headers denormalized onto it:

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-cell-6",
  "@type": "doc:TableCell",
  "doc:cellValue": "1",
  "doc:columnHeader": "a",
  "doc:rowHeader": "…",
  "doc:columnIndex": 0,
  "doc:rowIndex": 0,
  "nif:beginIndex": 212, "nif:endIndex": 213 }
```

This is what lets a query ask for "the Supply voltage row of the LM358B
column" without the consumer reconstructing the grid. Merged cells are
denormalized first, so every cell stands on its own.

A table with no header row has no `doc:columnHeader` on its cells, and
`doc:rowIndex` counts from its first row. A list of fields set `Label |
value` is one: each value's `doc:rowHeader` is its label. See [a list of
fields](../design/tables.md#a-list-of-fields-has-no-header-row).

Where a table's first columns are labels set down the rows they group, a
group over its sub-groups, a cell's `doc:rowHeader` names them all, group
first, then the row's own label: `FP2 Backplane / Conventional type /
7-module type (for master and expansion)`. A column spanning the same rows
as the group is read as more of the group, and named as the row's label
after it: `0.1 mL / white`.

A table set inside one cell of another (an acoustic row's nine columns of
figures in a property list) is a table of its own, right after the one it
sits in. Its cells carry the outer row's label as `doc:sectionLabel`
(`Acoustical Performance`), and the outer cell keeps only its own text.

A list wrapped to fill the page sets its columns again side by side:
`Competitor | Part no. | Competitor | Part no.`, each copy a record of its
own. Where the copies' value columns are named alike and each copy opens
with a key column named apart from them, a cell's `doc:rowHeader` is the
key of its own copy, not the row's first cell. `doc:rowIndex` and
`doc:columnIndex` stay the position on the page, so the cells sharing a
`doc:rowIndex` there are several records, one per copy.

A cell's offsets slice the projection to its `doc:cellValue`, which is its
text, so it carries no `nif:isString` repeating it. A value copied into a cell
by a merge has no place in the projection and no offsets.

Where the source stores a typed value under the text it shows — a
[workbook](../inputs/office-and-web.md#xlsx) does — the cell also carries it,
at full precision:

```json
{ "@type": "doc:TableCell",
  "doc:cellValue": "12%",
  "doc:cellDatum": { "@value": "0.12345", "@type": "xsd:decimal" } }
```

`doc:cellValue` stays what the author saw, and what the text projection reads;
`doc:cellDatum` is what to compute with. Other formats store text, and their
cells have none: a type guessed from text is a reading of the page, and
`45584888` may be an order number, `03/04/2026` either of two days.

A table that runs onto the next page is one `doco:Table` per page, and each
part after the first names the one before it with `doc:continues`. Its cells
are read as rows of one table: their `doc:columnHeader` comes from the first
part's header when this part has none, `doc:rowIndex` numbers on from the part
before, and a cell the page break cut takes its value, and so its row's
`doc:rowHeader`, from above the break. To collect a row, follow
`doc:continues` back to the first part and group by `doc:rowIndex`.

Because a cell is a node rather than a position in a grid, there is no merge
shape this format fails to represent. That is not true of
[`xhtml`](xhtml.md), where a region HTML cannot tile is dropped along with its
text — so where completeness matters more than fidelity to the drawn spans,
this is the format to read.

## Transcripts say who spoke and when

```json
{
  "@type": "doco:Paragraph",
  "doc:speaker": "Ada Park",
  "doc:startMs": 272404,
  "doc:endMs": 279512,
  "doc:evidence": "vtt",
  "nif:beginIndex": 146,
  "nif:endIndex": 193,
  "nif:isString": "Ada Park: I want to say this is my last review."
}
```

A turn read from a [transcript](../inputs/transcripts.md) carries its
speaker and where it sits in the recording, in place of the `doc:bbox` a
page would give it. The times are integers, in milliseconds from the start
of the recording, so "what was said in the fifth minute" is a numeric
comparison in any query language.

`doc:speaker` is the name as the file writes it, unresolved: `Speaker 2`
stays `Speaker 2`. The same name opens `nif:isString`, followed by `": "`, so
the speaker's mention spans `nif:beginIndex` to `nif:beginIndex` plus the
length of the name. That is the span to link to a person record. The times
never appear in the text, so they never become entities.

## Emails are threads of messages

```json
{ "@type": "foaf:Document",
  "dcterms:title": "RE: Pilot",
  "dcterms:creator": ["Lena Holt <lena@example.com>"],
  "dcterms:created": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:attachments": { "@type": "@json",
                       "@value": [ { "filename": "quote.pdf",
                                     "contentType": "application/pdf", "size": 48213,
                                     "sha256": "c41e09a7…" } ] } }

{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-message-7",
  "@type": "doc:Message",
  "doc:from": ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-2"],
  "doc:to":   ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-3"],
  "doc:sentAt": { "@value": "2026-07-17T13:48:00-05:00", "@type": "xsd:dateTime" },
  "doc:subject": "RE: Pilot",
  "doc:messageId": "3@example.com",
  "doc:inReplyTo": ["2@example.com"],
  "po:contains": [ "…-element-8", "…-element-9" ] }

{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-2",
  "@type": "doc:Mailbox",
  "doc:address": "lena@example.com",
  "doc:name": "Lena Holt" }
```

An [email](../inputs/email.md) is split into its messages: the file's own
and each one quoted in its body. Each is a `doc:Message` under the body,
side by side in reading order rather than nested as the quoting nests them,
so every message in a thread is one step from its sender. A message
contains its elements, starting with the paragraph that holds its header as
the text shows it.

Every sender and recipient is a `doc:Mailbox`, one per address in the
document, so every message a person sent or received points at the same
node. `doc:address` is the value to join on when linking a mailbox to a
contact record. Mailboxes are minted per document, like every other node,
so re-extracting one email never touches another's. A mailbox written with
a name and no address, as a quoted Outlook header often has, joins the
addressed mailbox of the same name when the document has one.

A quoted message's `doc:sentAt` has no offset, because the line that quotes
it states none. The file's own always has one.

A message that goes on after a quote nested in it, as a signature set below
the quoted thread does, goes on in its own `doc:Message`: the elements after
the quote are contained by the message they belong to, not by the one quoted
before them.

```json
{ "@id": "urn:fluree-doc-parse:reply-5c0e2a91d7b3-signature-31",
  "@type": "doc:Signature",
  "doc:signer": ["urn:fluree-doc-parse:reply-5c0e2a91d7b3-mailbox-4"],
  "nif:isString": "Best,\nKai\n\nKai Moreno\nExample Data Inc.\n1 Main Street, Springfield",
  "nif:beginIndex": 1180, "nif:endIndex": 1243,
  "po:contains": [ "…-element-32", "…-element-33" ] }
```

Each message's [signature](../inputs/email.md#signatures) is a
`doc:Signature` inside its `doc:Message`, containing the signature's
elements, with `doc:signer` pointing at the message's senders. Its span
covers its elements' text in the projection. A place named in a signature
is the signer's: an address under a name is that sender's office, not the
office of a company the message talks about, and `doc:signer` is the join
that keeps it there.

## Links are nodes

A hyperlink becomes its own node, referenced from the element whose text
carries it. A node rather than a property, because one paragraph can hold
several links and each has its own anchor — flattened onto the element they
would be a set of targets with no way to tell which words point where.

```json
{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-element-12",
  "@type": "doco:Paragraph",
  "doc:link": [ "urn:fluree-doc-parse:report-661511bb2b30-link-13" ],
  "nif:beginIndex": 1866, "nif:endIndex": 1910,
  "nif:isString": "Learn more at www.example.org/plan" }

{ "@id": "urn:fluree-doc-parse:report-661511bb2b30-link-13",
  "@type": "doc:Link",
  "doc:linkTarget": { "@value": "https://www.example.org/plan", "@type": "xsd:anyURI" },
  "nif:beginIndex": 1880, "nif:endIndex": 1910,
  "nif:isString": "www.example.org/plan" }
```

| term | what it carries |
|---|---|
| `doc:linkTarget` | an address outside the document, as an `xsd:anyURI` literal |
| `doc:linkPage` | a jump inside the document: 0-based page index |
| `nif:isString` | the anchor text |
| `nif:beginIndex` / `nif:endIndex` | the anchor's offsets into [`-f text`](text.md) |

Exactly one of `doc:linkTarget` and `doc:linkPage` is present. `doc:link` is
IRI-coerced in the context, so an element's links are nodes you can follow.
The target is a literal, not a node: a store that keys namespaces on an IRI's
path, as Fluree does, would otherwise keep one for every directory of every
address a corpus links to, and which ones is up to the documents. Which
documents point at a domain is a string test on it:

```sparql
SELECT DISTINCT ?doc
WHERE { ?link doc:linkTarget ?target ; doc:sourceDocument ?doc .
        FILTER (STRSTARTS(STR(?target), "https://www.example.org/")) }
```

The anchor's offsets are in the same space as every other offset in the graph,
so the interval lookup that finds an entity mention finds a link anchor too.
They are absent where the annotation covers something with no text of its own,
and on tables, whose projection joins cells with tabs — an offset into the
element's own text would index nothing there.

## IRIs and re-extraction

Every node IRI is the base IRI, a `-`, the kind of node and its number:
`urn:fluree-doc-parse:report-661511bb2b30-element-12`. The base is, in order:

1. `--base-iri`, when given;
2. else `--doc-iri`, so a document's nodes are named after it;
3. else `urn:fluree-doc-parse:`, the file's stem with anything outside
   letters, digits, `.`, `_` and `~` turned into `-`, and the first twelve hex
   digits of the file's SHA-256 (just the digits, from standard input).

The hash is there because names repeat: `report.pdf` and `report.docx` share a
stem, and so does every `report.docx` on a shared drive. Nodes minted under one
base are one set of nodes in a store, so two documents under the same base
would merge.

Nothing minted adds a `/`, `#` or `:` to the base, so every node of a document
lands in the namespace its base already sits in. Fluree splits an IRI into a
namespace and a name at the last of those, and encodes each namespace once; a
whole corpus minted this way costs one namespace, not one per document. A base
ending in `/`, `#` or `:` keeps its own namespace per document, and the `-` is
left off: `https://example.org/doc/7f3a/` mints
`https://example.org/doc/7f3a/element-12`.

`--doc-iri` stamps nodes with `doc:sourceDocument → <iri>`. That tag is what a
re-extraction retracts by: delete everything pointing at the document IRI,
insert the new graph, and the ledger holds exactly one extraction of that
document without a diff. Table cells are not stamped: a cell is always one
`po:contains` below its table, which is, and cells are most of a table-heavy
graph.

```bash
fdoc convert report.pdf -f doco --doc-iri urn:doc:finance-q3-report
```

See [Loading into a Fluree ledger](../integration/ledger-ingest.md).

## Pairing with text

`nif:beginIndex` / `nif:endIndex` index into the string
[`-f text`](text.md) produces — not into the Markdown, and not into
`nif:isString` concatenated. Generate both from the same run.

## Written a node at a time

`fdoc` writes the graph to its output as it makes it, a node at a time,
and never holds it whole: a table's cells are most of a large document's
graph, and a table of two million cells makes 800 MB of it. What is held
while it is written is the document's elements and, for each node, what it
contains, so DoCO converts what Markdown converts
([limits](../reference/limits.md#the-output-converted-to)).

A node is written before the nodes it contains (the document, its body,
then everything in it; a table before its cells), so the elements are
walked twice: once to find what each node contains, once to write. The
bytes are those of the graph built whole and pretty-printed, keys in order.

In a library, `fluree_doc_model::doco::write_doco(&elements, &opts, writer)`
writes it to any `std::io::Write`, buffered; `to_doco` returns the same
bytes as one `String`, which holds the whole graph.
