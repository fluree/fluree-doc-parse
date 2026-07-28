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
| `render-routed <path> [out]` | routed pages/regions to PNG crops with a splice manifest |

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
[Wiring the escalation tiers](../integration/escalation-tiers.md).
