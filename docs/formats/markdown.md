# Markdown

```bash
fdoc convert report.pdf              # -f md is the default
fdoc convert report.pdf -f md -o report.md
```

GitHub-flavored Markdown. The default because it is the projection you can
read without tooling, and the one an LLM consumes best.

## What it emits

| element | Markdown |
|---|---|
| `doco:SectionTitle` | `#` … `######`, from `level` |
| `doco:Paragraph` | a paragraph, blank-line separated |
| `doco:List` / `ListItem` | `- ` items |
| `doco:Table` | a pipe table with a header separator row |
| `doco:Figure` | a paragraph (an [anchor](../integration/anchors.md) token, where enabled) |

```markdown
# 1 Features

• Wide supply range of 3V to 36V (B, BA versions) • Quiescent current: 300μA/ch

|PART NUMBER (1)|PACKAGE|PACKAGE SIZE (2)|
|---|---|---|
|LM358B, LM358BA, LM2904B|D (SOIC,8)|4.9mm × 6mm|
```

## Links

A link is content, and a PDF keeps it where no glyph pass will find it: a
rectangle beside the content stream with an address behind it. Those
annotations are read, matched to the words they cover, and emitted inline.

```markdown
See [the filing](https://www.sec.gov/…) for detail.
```

Three forms come out of it:

| the page | the Markdown |
|---|---|
| linked text | `[the filing](https://…)` |
| text that *is* its own address | `<https://example.org/a>` |
| a jump to elsewhere in the document | `[Chapter 4](#page=12)` |

`#page=N` is 1-based, the fragment convention PDF viewers already use, and it
is the only form Markdown has for "elsewhere in this document". A jump that
lands on the page it starts from is not emitted: it takes a reader nowhere.

A link whose anchor sits on a picture rather than on text has no words to mark
up, so it appears in [`json`](json.md) and [`doco`](doco.md) and not here.
Anchors inside table cells are the same — a pipe table has no room for one.

Markdown and HTML sources carry their own links through unchanged, so
`fdoc convert notes.md` round-trips them. DOCX and PPTX do not yet.

**This costs benchmark score, and is emitted anyway.** Ground truth
transcribed from a visible page has no link markup, so every address we
recover is counted as an insertion error — see [where our output differs from
the reference](../benchmarks/where-we-differ.md).

## What it loses

Everything geometric. No pages, no bounding boxes, no
[evidence](../concepts/provenance.md), no char offsets. Markdown has nowhere
to put them, and encoding them in comments would produce a file that is
neither good Markdown nor a good data format.

If you need any of that, use [`json`](json.md) or [`doco`](doco.md) — and note
that [`text`](text.md), not this, is the string
[`doco` offsets](../concepts/text-projection.md) index into. Markdown's
`#` markers and pipe characters shift every offset.

## Tables carry the grid as detected

A Markdown pipe table cannot express a rowspan, and rather than fake one by
repeating a spanned value into every row it covers, the rows are exactly what
[`json`](json.md) reports in `cells`: the value sits where the text was laid
out, and the positions it spans are empty. A reference encodes a rowspan as
one cell, not N copies.

If you want rows that stand on their own, use [`doco`](doco.md), whose
`doc:TableCell` nodes carry the merged value resolved together with their row
and column headers. If you want the spans themselves, use
[`xhtml`](xhtml.md), which emits real `rowspan` and `colspan`.

A `|` inside a cell is escaped as `\|`, a `\` as `\\`, and a newline becomes a
space, so a row always has the column count its header promises. The backslash
has to be escaped as well as the pipe: without it a cell whose text *ends* in
one turns the following delimiter into `\|` and swallows it.

That means a lookbehind is not enough to parse this — `(?<!\\)\|` refuses to
split after a cell ending in an escaped backslash, because the character before
the delimiter really is a `\`. Scan instead, treating `\` as consuming the
character after it:

```python
def cells(row):                       # row includes its leading and trailing |
    out, cur, esc = [], [], False
    for ch in row.strip()[1:-1]:
        if esc:   cur.append(ch); esc = False
        elif ch == "\\": esc = True
        elif ch == "|": out.append("".join(cur)); cur = []
        else: cur.append(ch)
    out.append("".join(cur))
    return out
```

Page
[furniture](../design/furniture.md) is stripped **before** table assembly
rather than after, which keeps headers and footers out of the grid entirely.

## Known roughness

Bullet glyphs that the layout pass read as inline text rather than list
markers stay inline, as in the `• Wide supply range…` line above. The marker
detector requires a marker to be marker-shaped rather than merely short, so a
multi-column bullet run set as flowing text stays a paragraph.
