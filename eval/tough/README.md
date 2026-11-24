# Tough cases

Real documents the engine once read wrong, each with the facts a fix made
true, so a later change cannot quietly undo it.

```
eval/tough/check.py --fetch     # first run: download the public documents
eval/tough/check.py             # every case, tracked and private
eval/tough/check.py nist-sec    # one case
eval/tough/check.py --fdoc PATH # another binary, e.g. the one before a fix
```

It runs `fdoc convert --no-escalate` to DoCO and to text for each document and
exits non-zero when a check fails or a document is missing.

## Why a set of our own

The benchmark scores Markdown over 200 documents from one publisher's mix. A
fix to a table it never shows moves no number there, and a later change can
undo the fix without moving one either. The two fixes these cases began with
(rows a word processor draws without ruling, and a table split by a page
break) changed no benchmark output at all. Here they are checked by name.

## Layout

| path | what | tracked |
|---|---|---|
| `cases/<id>.json` | a public document and its checks | yes |
| `docs/` | the public documents, fetched by `--fetch` | no |
| `private/<id>.json` | a document that cannot be published, by local path | no |
| `fixtures/` | documents made for a case, from invented text, with their source | yes |

Public documents are fetched from their publisher, never redistributed, and
checked against the SHA-256 their checks were written on. A different copy is
reported, not failed: the publisher may have revised it, and the checks may
then need revising too.

A case on a fixture names it by `"path"` relative to the case file, and its
notes say how to make it again.

Private cases follow the same format with `"path"` (absolute, `~`, or relative
to the case file) in place of `"url"`. Their documents and their checks quote
text that must not enter the repository, so the whole directory stays local.

## A case

```json
{
  "document": {
    "file": "nist-sec.pdf",
    "url": "https://www.nist.gov/document/sec-protocol",
    "sha256": "988f0ab6…",
    "about": "what the document is, and what makes it hard"
  },
  "notes": ["history the checks do not capture"],
  "checks": [
    {
      "id": "formulation-spans-its-samples",
      "found": "2026-11-17",
      "issue": "what was wrong, in the document's terms",
      "row": {"Proteins": "Amgen IgG2", "Formulations": "10 mM sodium acetate, pH 5.0"}
    }
  ]
}
```

Every document is held to the offset contract: each element's `nif:isString`
(a cell's `doc:cellValue`) is exactly what its `nif:beginIndex` and
`nif:endIndex` cut from the text projection. The checks a case adds:

| kind | passes when |
|---|---|
| `row` | some table row has each of these values under these column names |
| `no_row` | no table row has them all |
| `no_column_header` | no cell's column name contains this text |
| `cell` | some cell has this `value`, and the `column`, `row` and `section` names given |
| `text` | the text projection contains this text, exactly `count` times where one is given |
| `no_text` | the text projection does not contain this text anywhere |
| `element` | some element has this `type`, exactly this `text` (its `nif:isString`, line breaks and all) and, where given, this `evidence`; exactly `count` elements do where one is given, `0` for one that must be gone |
| `section` | some section is headed by exactly this `title` and, where given, sits at this `level` and directly inside the section headed by `within` |
| `no_section` | no section is headed by a title beginning with this text |
| `under` | an element with exactly this string (`text`) falls in the section headed by `section` |
| `folio` | the `page` (its place in the file, from 1) carries this printed `value` in `doc:pages`, or none where it is `null` |
| `same_every_run` | this many more conversions give the same DoCO, byte for byte |

A row is the cells sharing a `doc:rowIndex` in one table, a table split by a
page break counting as one (its parts are followed back by `doc:continues`).
A column name is the cell's `doc:columnHeader`, a stacked header's labels
joined with ` / `; a cell under no name is keyed by its column, `#0`. Text is
compared with runs of whitespace as one space.

`"status": "open"` marks a check that records a known failure. It does not
fail the run, and the run reports it as `NOW PASSES` once it does, at which
point drop the status.

## Adding a case

When a fix comes from a real document:

1. Write the checks from the output the fix produces, in the document's own
   words: the row a reader would point at, the name that must not appear.
2. Run them against the binary before the fix (`--fdoc`) and confirm they
   fail. A check that passes either way guards nothing.
3. A public document goes in `cases/` with its URL and SHA-256; anything else
   goes in `private/`.

Problems found but not fixed go in as `open` checks, so the set doubles as the
list of what is known to be wrong.
