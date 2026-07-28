# fdoc forms

Extract AcroForm fields — name, type, value and placement — as JSON.

```bash
fdoc forms document.pdf
```

## Why this is a separate command

Filled-in form values live in **widget annotations**, not the content stream.
A completed form therefore converts as its blank template: `fdoc convert` sees
the glyphs that were drawn, and the value someone typed into a field was never
drawn at all.

This command reads the annotation layer instead, so what a user entered comes
back.

## Output

A JSON array, one object per field, with placement in render coordinates so
values can be positioned over a rendered page. A document with no AcroForm
returns `[]`.

```bash
fdoc forms application.pdf | jq -r '.[] | "\(.name)\t\(.value)"'
```

## Scope

PDF only. Given another format the command fails cleanly rather than returning
an empty array — an empty result would be indistinguishable from a PDF that
genuinely has no form.

XFA forms (the XML-based Adobe variant) are not supported; only AcroForm.

## Related

- [`fdoc convert`](convert.md) for the document body
- Drawn checkboxes — the ones stroked into the content stream rather than
  declared as fields — are read by the layout pass as list markers instead,
  and appear in normal converted output.
