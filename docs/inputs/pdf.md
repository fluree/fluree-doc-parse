# PDF

The geometric path. A PDF declares no structure — it places glyphs — so
paragraphs, headings, lists, tables, columns and reading order are all
inferred from position. See [the pipeline](../design/pipeline.md) for how.

## What you get that no other source gives

- **Bounding boxes** on every element, in PDF user units with a top-left
  origin — directly usable as CSS.
- **Character-level geometry**, which is what makes [entity
  overlay](../integration/entity-overlay.md) work: a char offset resolves to a
  glyph, and a glyph range to merged per-line rectangles.
- **Per-glyph rotation**, derived from the text transform. A 90° label is an
  axis title or a vertical dimension, not body text, so orientation buckets
  reading order before it runs. This is what keeps engineering drawings
  readable and why they [do not
  escalate](../concepts/escalation.md#where-escalation-is-wrong).
- **AcroForm fields** — see [`fdoc forms`](../cli/forms.md).
- **The outline (bookmark) tree** — a near-ground-truth heading signal, and
  one no other engine tested uses.
- **Link annotations.** A hyperlink is a rectangle and an address beside the
  content stream, so a glyph pass sees the anchor's words and never what they
  point at. They are read, matched to the words they cover, and emitted in
  every format that has room for them — see
  [Links](../formats/markdown.md#links).
- **The document information dictionary** — `/Title`, `/Author`,
  `/CreationDate` and `/ModDate` — on the [DoCO](../formats/doco.md)
  document node as `dcterms:title`, `dcterms:creator`, `dcterms:created` and
  `dcterms:modified`. A date carries its offset where the file states one
  other than zero; `Z` and no offset read alike, so neither is written. What
  the file declares can be stale — a title of `Microsoft Word - draft3.doc`
  is what that file says — and a title guessed from the largest line on the
  first page would be a reading of the page, so none is.

## Text fidelity

These cleanup passes run before anything else:

**Faux-bold dedup.** PDFs fake bold by drawing text twice with a small offset.
Left alone this produces `検検討討会会のの構構成成` instead of
`検討会の構成`. A glyph drawn again where the same glyph already stands is
dropped however far apart the two draws are: a document printed with its
highlighting draws each line once under the highlight and once over it, and
read twice, every line came out with each letter doubled: `RReeccoorrdd`.

**Underlines struck over text.** A line printer underlines by printing the
line, then printing `_` under the letters it stresses. Read by position, the
two interleave: `_D_O_ _N_O_T`. A run of underscores struck over another
mark is dropped, and the words read whole. Underscores struck over nothing
are a blank left to be filled in, and stay.

**Tables drawn with characters.** The same printers draw a table's ruling
with `|` (or `¦`) down each column's edge and a line of `_` or `-` between
rows. Those characters are read back into the ruling they draw and leave the
text, so the table is read like any ruled table, header row and all. A blank
printed line inside a row parts it where the form means it to, so a step's
instruction sits beside its result blank, `Weight:` beside `________ kg`,
under the step's number.

**Highlighting.** A reviewer's highlighting is drawn as a filled band fitted
to each line it marks. Bands like that are not structure, so they are set
aside before tables and figures are looked for: read as structure, a page of
marked-up prose looked like a bar chart, or like a table of its numbered
clauses.

**NFKC normalization.** Ligatures arrive as single codepoints; searching
un-normalized output for `profile` finds one occurrence in six. Normalization
can change string length, so the engine keeps a bijective map between raw
glyph offsets and normalized text offsets. Superscripts and subscripts are
kept as printed (`m³/h`, `H₂O`) rather than folded to plain digits.

Measured Unicode resolution: **99.97%** on a 200-document Latin corpus,
**99.73%** on CJK, with **zero** replacement characters on Japanese and
Chinese government documents.

## When PDF is hard

Two failure modes, both detected rather than guessed:

- **The text is pixels.** A scanned page has no glyphs to analyze.
- **The text is garbage.** Broken CID fonts yield glyphs whose Unicode is
  unknown or wrong — the layout can be perfect and the text worthless.

`fdoc triage` reports both. See [the router](../design/router.md).

## Encrypted and malformed files

`extract_file` returns `ExtractError::Parse` for a PDF that cannot be opened.
A panic on real input is treated as a release blocker rather than a bug
report, so malformed files fail as errors you can catch.

## Performance

~7 ms per document for the deterministic tier, ~2.4 ms per page for parsing
alone, ~420 pages/s. There is no model to load, so the first document costs
what the thousandth does.
