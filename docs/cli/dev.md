# fdoc dev

Pipeline internals, for debugging extraction.

```bash
fdoc dev <SUBCOMMAND> [args]
```

> **Not a compatibility surface.** These expose intermediate layout state —
> raw glyphs, assembled lines, blocks, detected furniture, table geometry.
> Their output formats may change in any release. Do not parse them in
> anything you ship.

## Subcommands

**Text and glyphs**

| command | shows |
|---|---|
| `probe <dir>` | T0/T1 metrics over a corpus: parse rate, Unicode %, panics |
| `glyphs <pdf> [page] [y0] [y1]` | raw glyphs in draw order, optionally in a y-band |
| `lines <pdf> [page]` | assembled lines (layout pass 1) |
| `find <pdf> <text>` | resolve a text span to overlay rectangles |
| `pair <pdf> <text>` | measured gap around a character pair |

**Layout**

| command | shows |
|---|---|
| `gaps <path>` | horizontal gap distribution (word/block-split tuning) |
| `leading <path>` | vertical gap distribution (leading vs paragraph breaks) |
| `blocks <pdf> [page]` | paragraph blocks with furniture stripped |
| `columns <pdf> [page]` | column regions with x-occupancy profiles |
| `furniture <pdf>` | detected headers, footers and watermarks |

**Structure**

| command | shows |
|---|---|
| `outline <pdf>` | the PDF bookmark tree (heading ground truth) |
| `links <pdf>` | link annotations, and the anchor text each one covers |
| `headings <pdf>` | detected headings with evidence and level |
| `weights <pdf>` | glyph weight histogram by font size (bold detection) |
| `rules <pdf> [page]` | ruling lines and fills (table geometry) |
| `tables <pdf> [page]` | detected table grids with cell text |
| `aligned <pdf>` | aligned-table candidates before/after corroboration |
| `figures <pdf> [page]` | chart and diagram regions inferred from drawn shapes |
| `fidelity <pdf>` | our own text checked back against the page's glyphs |

**Render**

| command | shows |
|---|---|
| `render-pages <path> [out]` | every page to PNG at 2× |
| `render-routed <path> [out] [--ocr-regions] [--stored-resolution]` | routed pages/regions to PNG crops with a splice manifest; `--ocr-regions` adds the regions too small for a model, and `--stored-resolution` renders each crop at the resolution of the image it shows (2× to 300 dpi, the page it is cut from drawn within 64 million pixels, below 2× if need be), both for an OCR engine. A bare image (PNG, JPEG, TIFF) gets one crop, `<stem>_p0_full.png`: the image upright, over white, at `scale` 1. A DOCX's images get one crop each, however many figures draw one, `<stem>_m<n>.png` named by the first (`<stem>_m<n>_<k>.png` for a figure's later images), `kind: "media"` with that `figure`, all its `figures` and its package `part`; so do the pictures anchored to an XLSX's cells (figure `figure-<sheet>-<n>`), and an e-mail's images shown in its body likewise (an input is told apart as `convert` tells it: an e-mail or a transcript by its content whatever its name, an image by its first bytes), their `part` the `cid:` URL the message names them by (`attachment <n>`, its place among the message's files, where no Content-ID names the part alone); past what one document may have read (256 images, 512 million pixels) the rest are not cropped |

**Timing**

| command | shows |
|---|---|
| `timings <path>` | wall clock per pipeline stage, in one process |

`timings` exists because the evaluation harness cannot answer the question: it
spawns a process per document, so startup and page cache dominate a corpus
that parses in milliseconds. Stage totals from a single process are the only
honest way to see which stage is worth attention.

## Typical use

Why did a heading get missed?

```bash
fdoc dev outline report.pdf                 # does the document declare it?
fdoc dev headings report.pdf                # what did we detect, on what evidence?
FDOC_HEADING_SOURCES=1 fdoc dev headings report.pdf   # per-detector attribution
```

Why is a table wrong?

```bash
fdoc dev rules report.pdf 12                # is the geometry there?
fdoc dev tables report.pdf 12               # what grid came out of it?
fdoc dev aligned report.pdf                 # what did corroboration reject?
```

Why is the text spaced oddly?

```bash
fdoc dev gaps report.pdf                    # where is the word-split threshold?
fdoc dev pair report.pdf "ab"               # what gap does this pair actually have?
```

`render-routed` is also how you produce the crops a model tier reads — see
[Wiring the escalation tiers](../integration/escalation-tiers.md). With
`FDOC_STRUCTURE_RESULTS` set, it still writes the table crops those
structure readings veto, for the structure reader, and marks them
`"vetoed": true` in the manifest: a run that always passes those structure
readings never sends them to a deep reader.

It chooses its crops as `convert` does, config file included: with
`escalation.on_column_doubt` on in the config file in effect (the
`.fdoc/config.toml` nearest the working directory, or the per-user one where
there is none; the two are not merged), a page in column doubt is written
whole in place of its region and table crops, and a note on stderr says so.
`FDOC_ESCALATE_COLUMNS=0` turns that off for one run; producing the crops of
a benchmark cache, set it, so a file left on the machine cannot change them.
