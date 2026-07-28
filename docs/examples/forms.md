# Form fields

A completed PDF form converts as a blank one. This is not a bug in the
extractor — the values a user typed live in widget annotations, not in the
page's content stream, so no amount of layout analysis will find them. They
have to be read from a different place in the file.

```bash
fdoc forms filled.pdf
```

```json
[
  { "page": 0, "name": "f1_01[0]", "kind": "Tx", "value": null,
    "bbox": { "x0": 58.6, "y0": 118.0, "x1": 576.0, "y1": 132.0 } },
  { "page": 0, "name": "c1_1[0]", "kind": "Btn", "value": null,
    "bbox": { "x0": 73.0, "y0": 180.2, "x1": 81.0, "y1": 188.2 } }
]
```

| field | meaning |
|---|---|
| `page` | 0-based |
| `name` | the AcroForm field name, as the form's author wrote it |
| `kind` | the PDF field type verbatim: `Tx` text, `Btn` button/checkbox/radio, `Ch` choice, `Sig` signature |
| `value` | the filled value, or `null` for an unset field |
| `bbox` | the widget's rectangle, in render coordinates |

## Map field names once per template

`f1_01[0]` tells you nothing on its own, and pairing each field with the
nearest text element is unreliable: a form's visible captions usually sit
inside its ruled boxes, so the layout pass reads them as table cells rather
than as labels beside the widgets.

Work with the shape of the problem instead. **A form is a template** — the
field names are stable across every filing of it, so map them once and reuse
the map.

```python
import json
import subprocess


def run(cmd):
    return json.loads(subprocess.run(cmd, capture_output=True, check=True).stdout)


W9 = {
    "f1_01[0]": "name",
    "f1_02[0]": "business_name",
    "c1_1[0]":  "classification.individual",
}

def values(path, field_map):
    return {field_map[f["name"]]: f["value"]
            for f in run(["fdoc", "forms", path])
            if f["name"] in field_map}
```

Build that map once per template, with `bbox` and a rendered page to see what
you are naming:

```bash
fdoc render w9.pdf ./pages              # PNGs at 2x
fdoc forms w9.pdf | jq -r '.[] | "\(.name)\t\(.page)\t\(.bbox.x0),\(.bbox.y0)"'
```

Multiply the box by 2 to find the widget on the render. It is a few minutes
per template, and then it runs unattended over every filing of that form.

## `null` means unset, for every kind of field

The extractor normalizes two PDF spellings of "nothing" into `null`: an empty
string, and the `Off` state a PDF uses for an unchecked box. So `value` is
never `""` and never `"Off"`.

That makes checkboxes simpler than the PDF spec suggests. A checked box
carries its **on-state name**, which is whatever the form's author chose —
`"Yes"`, `"1"`, `"X"` — so you cannot compare against a fixed constant, but
you do not need to:

```python
checked = f["kind"] == "Btn" and f["value"] is not None
```

Every field in a blank template is `null`. That makes for a useful smoke test:
if a form you believe is filled comes back all `null`, its values were
flattened into the page when it was saved, and no annotation layer survives to
read.

## Coordinates match `-f json`

Both commands report boxes in PDF user units with a **top-left origin**, so a
widget rectangle and an element rectangle are directly comparable — no flip,
no scale.

That is what lets you draw filled values over a rendered page: multiply both
by 2 for the default 2× render and they land where they belong.

## When there is nothing to read

Most PDFs have no AcroForm at all and produce `[]`. That includes scanned
forms, where the "fields" are ink on an image and the only route is [a model
tier](../integration/escalation-tiers.md) over the rendered page. `fdoc
triage` will have already told you such a page is `Scanned`.

```bash
fdoc forms report.pdf
[]
```
