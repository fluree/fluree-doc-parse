# Examples

Working recipes for the things people actually do with a parsed document.

| page | what it does | language |
|---|---|---|
| [Chunking for retrieval](chunking.md) | split on real section boundaries, keep page numbers for citation | Python |
| [Finding text on the page](spans.md) | a match in the text → the element → its page and box | Python |
| [Tables to rows and spans](tables.md) | `cells` → CSV, or real `rowspan`/`colspan` | Python |
| [Batch conversion](batch.md) | a directory of documents, and what to do with the failures | shell |
| [Pricing a deployment](triage-cost.md) | how many pages would need a GPU, before you buy one | shell |
| [Form fields](forms.md) | AcroForm values, which no other output carries | Python |

For loading the graph into a ledger, see
[Loading into a Fluree ledger](../integration/ledger-ingest.md). For running
the model tiers, [Wiring the escalation tiers](../integration/escalation-tiers.md).

## Calling it from any language

`fdoc` is a single static binary that reads a file and writes one of five
formats to stdout. There is no Python package and no FFI — you run it as a
subprocess and parse what comes back, which is a few lines in any language.

For the types rather than the JSON, use the
[Rust crates](../getting-started/rust-library.md).

### Python

```python
import json
import subprocess

def convert(path, fmt="md"):
    out = subprocess.run(
        ["fdoc", "convert", path, "-f", fmt],
        capture_output=True, check=True,
    ).stdout
    return json.loads(out) if fmt in ("json", "doco") else out.decode("utf-8")
```

`check=True` raises on a non-zero exit. The error text is on stderr:

```python
try:
    doc = convert("scan.pdf", "json")
except subprocess.CalledProcessError as e:
    print(e.stderr.decode())      # error: scan.pdf: parse: Invalid
```

### Node

```js
import { execFile } from "node:child_process";
import { promisify } from "node:util";
const run = promisify(execFile);

async function convert(path, format = "md") {
  const { stdout } = await run("fdoc", ["convert", path, "-f", format],
                               { maxBuffer: 256 * 1024 * 1024 });
  return stdout;
}
```

**Set `maxBuffer`.** Node's default is 1 MB and it fails the call rather than
truncating. A long report easily exceeds that in `-f json`.

### Shell

```bash
fdoc convert report.pdf -f json | jq -r '.[] | select(.type=="doco:SectionTitle") | .text'
```

## Two rules

**Pick the format that already holds what you need.** The five formats are
projections of one element list, not conversions of each other. Re-deriving
structure from Markdown that `-f json` would have handed you loses information
for no reason — see the [format matrix](../formats/README.md).

**Offsets belong to `-f text` and nothing else.** `nif:beginIndex` in the
[DoCO graph](../formats/doco.md) counts characters in the
[`-f text`](../formats/text.md) projection. Resolve them against Markdown or
XHTML and every highlight lands further off down the page. [Finding text on
the page](spans.md) does it correctly.

### Offsets in JavaScript

Offsets count **Unicode code points**. Python's `str` indexing agrees, so
slicing works directly. JavaScript's `String.prototype.slice` counts UTF-16
code units, which differ for anything outside the Basic Multilingual Plane —
emoji, rare CJK extensions, some historic scripts. Convert once with
`Array.from(text)` and index that.
