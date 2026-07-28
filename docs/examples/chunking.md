# Chunking for retrieval

The default chunker in most RAG stacks slices text every N characters with an
overlap. It is a reasonable thing to do when all you have is a string, and it
is the wrong thing to do when you have structure: it cuts tables in half,
separates a heading from the paragraph it introduces, and produces chunks
whose provenance is a byte range nobody can cite.

With `-f json` you have reading order, heading levels, and a page number per
element. That is enough to chunk on the document's own boundaries.

## The chunker

```python
import json
import subprocess


def elements(path):
    out = subprocess.run(
        ["fdoc", "convert", path, "-f", "json"],
        capture_output=True, check=True,
    ).stdout
    return json.loads(out)


def chunks(path, max_chars=1500):
    """Yield chunks that never straddle a heading boundary."""
    stack, body = [], []

    def flush():
        if not body:
            return None
        pages = sorted({e["page"] for e in body})
        chunk = {
            "heading_path": list(stack),
            "text": "\n\n".join(e["text"] for e in body),
            "pages": pages,
            "first_page": pages[0],
        }
        body.clear()
        return chunk

    for el in elements(path):
        if el["type"] == "doco:SectionTitle":
            out = flush()
            if out:
                yield out
            level = el.get("level", 1)
            stack[level - 1:] = [el["text"]]
        else:
            body.append(el)
            if sum(len(e["text"]) for e in body) >= max_chars:
                yield flush()

    out = flush()
    if out:
        yield out
```

```python
for c in chunks("report.pdf"):
    head = " > ".join(c["heading_path"]) or "(no heading)"
    print(f"[p{c['first_page']}] {head}  ({len(c['text'])} chars)")
```

```
[p0] 1 Features  (608 chars)
[p0] 2 Applications  (489 chars)
[p0] 3 Description  (2014 chars)
[p3] 5 Specifications 5.1 Absolute Maximum Ratings > 5.2 ESD Ratings  (723 chars)
[p4] 5 Specifications 5.1 Absolute Maximum Ratings > 5.3 Recommended Operating Conditions  (1791 chars)
```

## What the structure buys

**The heading path is the context.** `stack[level - 1:] = [text]` maintains a
breadcrumb from the heading levels, so a chunk about ESD ratings knows it
belongs to Specifications. Embed the path with the text and a query for
"maximum ESD" matches the section that answers it rather than the twelve
others that mention ESD in passing.

**`page` is a citation.** Every chunk carries the pages it came from, so an
answer can point at page 4 of the source rather than at chunk 37. That is the
difference between a citation a user can check and one they have to trust.

**Tables stay whole.** A `doco:Table` element is a single element with its
cells already joined, so it enters a chunk intact. This is why chunks can
exceed `max_chars`: the size check runs after appending, deliberately, because
half a table is worse than an oversized chunk. Expect a long specification
table to produce a single chunk several times your limit.

## Adjustments worth making

**Filter figure fragments.** Text inside charts arrives as `doco:Figure`
elements in page order, which is not a reading of the chart — see [Figures
come in groups](../formats/json.md#figures-come-in-groups). If your documents
are chart-heavy, drop them, or group them by `figure` id and keep the group as
one chunk:

```python
if el["type"] == "doco:Figure":
    continue
```

**Trust `evidence: "outline"` headings most.** Those came from the PDF's own
bookmark tree rather than from a font-size guess, so they are near ground
truth. If a document has them, you can chunk on those alone and ignore
detected headings entirely:

```python
outline_only = [e for e in elements(path)
                if e["type"] != "doco:SectionTitle" or e["evidence"] == "outline"]
```

See [Provenance and evidence](../concepts/provenance.md) for the full ladder
and what each rung is worth.

**Non-PDF sources need no changes.** DOCX, HTML and Markdown produce the same
element list with the same heading levels; they simply have no `bbox`, and
`page` is `0` throughout. The chunker above never touches `bbox`, so it works
unmodified — which is the point of one element model.
