# Scanned PDFs: a generated set and its check

Pages with no text layer, and a born-digital page with a screenshot pasted
into it, each with the facts drawn on it. The set is written by a script
rather than kept here: 58 pages of pixels are about 40 MB, and the script
writes the same bytes on every run.

```
uv run eval/scanned/make_scans.py                       # the set, into eval/scanned/corpus/ (not tracked)
cargo build --release
eval/scanned/check.py --models DIR                      # both runs, the tables, about half a minute
eval/scanned/check.py --no-ocr                          # the routing half only: no OCR assets needed
eval/scanned/check.py --models DIR --show letter-2.fax-std   # one document's text and its misses
eval/scanned/check.py --models DIR --baseline eval/scanned/baseline.json
eval/scanned/check.py --models DIR --baseline eval/scanned/baseline.json --allow-digest-mismatch   # another platform's set
```

`DIR` is the OCR assets directory `tools/ocr-assets/build.py` builds
(`--models`, or `FDOC_OCR_MODELS`). `--fdoc` (or `FDOC_BIN`) checks another
binary. The numbers and what they say are on the [scanned PDFs
page](../../docs/benchmarks/scanned.md).

## The set

Six business documents are drawn as born-digital PDFs with reportlab's
standard fonts: two letters, two statements with a ruled transaction table,
two invoices with an unruled item table, in Helvetica, Times and Courier.
Each is rasterised by pdfium, degraded, and written back as an image-only PDF,
once per profile, so every profile is measured on the same six pages:

| profile | what it is |
|---|---|
| `clean-300`, `clean-200`, `clean-150`, `clean-100` | 8-bit grey at that dpi, sensor noise only |
| `skew-300` | the sheet fed 0.8 to 2 degrees off square |
| `noisy-300` | grey paper, soft focus, heavy noise and speckle |
| `fax-200` | 1-bit, thresholded from a noisy scan |
| `fax-std` | 1-bit at fax standard resolution, 200 by 100 dpi: non-square pixels |
| `jpeg-150` | stored as a quality-35 JPEG |

The `pasted` documents are two born-digital letters with a small application
window pasted under the body, each written twice, the window captured at 96
and at 192 pixels per inch: four pages, two by two the same letter. Their
facts are marked `"where": "page"` (in the text layer) or `"where": "pixels"`
(only in the screenshot).

Beside each `<id>.pdf` is `<id>.gold.json`: the profile, every fact drawn
on the page with its kind (`name`, `id`, `amount`, `date`, `phrase`), its
headings, and on a pasted page the screenshot's box. The headings are the
lines the generator draws set apart in bold outside a table (a letterhead, a
signature, `INVOICE`, `Bill to:`, a closing balance): what fdoc makes
headings of on the born-digital page. A letter's `Re:` line, a field that
labels its reference, is not one. Each fact is said exactly
once on its page, by the rules the check finds it by: the generator draws a
page again from the next random source while it says one twice, so a fact
read wrong in one place is not credited by another place that says the same.
`index.json` lists the documents. Names, companies and places are invented.

The dependency versions are pinned in the script's header and the
interpreter is one uv manages, so a changed version shows up as a changed set
digest, not as an unexplained score. The bytes are the same from run to run
on one machine, not across platforms: the interpreter's zlib and the pdfium
and Pillow wheels differ from one to another.

## What is checked

Each document is converted to DoCO twice: as shipped, with `--no-escalate`
and no OCR, and with `--ocr oar-ocr`. Both runs use an empty fdoc
configuration: fdoc runs in an empty temporary directory that is also its
`FDOC_HOME`, and since it would still read a `.fdoc/config.toml` in any
directory above that one, the check asks `fdoc config path` first and stops
if it names a file. The text projection (`-f text`) is rebuilt from the DoCO
nodes' strings at their character offsets, so each page is read once per run
and the time per page is the time of that one reading. A fact is found
**strictly** when its text occurs in the text projection, whitespace runs
compared as one space, and **as a whole word** when that occurrence is not
part of a longer word or number. That is recall only: text the engine
invents, words no page holds, costs nothing, and only the headings gate
anything read beyond the facts.

The run without OCR is also a gate, and fails the check when:

* an image-only page is not in `doc:unreadPages` with the reason `Scanned`;
* a born-digital page is in it;
* a pasted page's screenshot is not in `doc:unreadRegions` (its reason is
  not checked: at 300 by 150 points the screenshot is under a tenth of the
  page, so it is listed as `Small`, a region the router would not send to a
  model either);
* a pasted page's headings (`doco:SectionTitle`, `doco:Title`) are not its
  gold's, word for word (their order aside);
* a fact only pixels hold appears in the text.

The OCR run fails it too when a page or a region is left unread, or when a
born-digital page is read whole (an OCR reading of crop `full`) rather than
its screenshot alone.

`baseline.json` records, per document, the facts the OCR run misses as a whole
word (`missed`), the number of headings it reads (`headings`), which of its
gold's headings they include (`gold_headings_read`) and the others, the lines
read as headings that the gold does not set apart (`other_headings_read`,
listed as often as read, so a heading of the gold read twice is listed there
once), the headings as normalised text and sorted; and the digest of the set
it was written on (`corpus_sha256`). With `--baseline` the check fails when a
fact found then is missed now, when a heading of its gold read as one then is
not now, or when a heading other than the gold's is read now that was not
then, or more often; on a document it does not list, which would otherwise
pass the headings unchecked, and on one it lists that the set no longer
holds; and on a baseline that lacks one of the three lists, whose gate would
otherwise not run; and when the set's digest is not the baseline's: another
set's pixels read differently, so a fact lost may be theirs and not fdoc's.
`--allow-digest-mismatch` compares all the same, with a warning, for a set
written on another platform. The headings are gated on both sides
because most scans read fewer than their gold: a body line turned into a
heading brings the count nearer the gold's, and would pass a gate on the
count. The count is recorded but not gated (it is the two lists' lengths
summed), and the check counts the facts newly found and the documents that
read more of their gold's headings or fewer others. The
headings are gated per document against the baseline, not against the gold:
OCR reads a scan's headings poorly today (see the scanned PDFs page), and the
gate is there to keep it from getting worse unnoticed. A heading other than
the gold's that is read with one letter changed is a new one, and fails.
When a change finds more, check it with `--baseline` first, then record it
with `--write-baseline eval/scanned/baseline.json` in the same commit, and
the new numbers on the scanned PDFs page. The two options cannot name the
same file in one run: the baseline would be written before it is read.
