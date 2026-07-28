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

## Text fidelity

Two cleanup passes run before anything else:

**Faux-bold dedup.** PDFs fake bold by drawing text twice with a small offset.
Left alone this produces `検検討討会会のの構構成成` instead of
`検討会の構成`.

**NFKC normalization.** Ligatures arrive as single codepoints; searching
un-normalized output for `profile` finds one occurrence in six. Normalization
can change string length, so the engine keeps a bijective map between raw
glyph offsets and normalized text offsets.

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
