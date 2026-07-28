"""Read the pipeline's own escalation crops with Gemini, into the tier cache.

Only what the triggers asked for. `fdoc dev render-routed` emits a crop per
routed region, and `table::suspect_tables` per doubtful grid; over the 200
benchmark documents that is 106 crops on 85 documents, so the other 115
documents are never sent anywhere. The crop's name carries what it is, and
what it is decides the prompt:

  _tN    a table whose detected structure disagrees with itself
  _rN    a region the text layer cannot read -- a raster chart or figure
  _full  a page with no usable text layer at all

The distinction matters more than it looks. Asked to read a bar chart *as
data*, a model rightly refuses: the bar heights are not printed, and deriving
them from pixels is estimation. Asked to *transcribe what is printed*, the
same model returns the legend, the tick labels and the axis titles -- which
is what the region actually contains and what the page lost. One prompt
recovers a document the other cannot.

A region crop is the hard case, because the router does not know what it
sent. Leaving the model to decide loses the tables: bench 110 (a 26-row
viscosity table) and bench 122 (a reagent grid) both came back as one value
per line and scored TEDS 0.0. So the crop carries a `table` flag from the
layout detector, which boxes both with 0.98 and 0.96 confidence, and the
prompt states what the image holds instead of asking. Where the detector is
silent the model still decides, and the chart branch still refuses to read a
value off a wedge.

A whole page is asked for Markdown rather than lines, because it is the one
crop that owns a document's structure as well as its text. Bench 141, a
two-column poster with no text layer, scored 0.309: read across the columns
instead of down them, and with no heading anywhere, because a page reading
arrives as one `text` block and only a `title` label becomes a heading.
Markdown gives the headings a channel that survives the splice, and naming
columns in the prompt fixes the order — 0.309 to 0.987, one call, no retry.

Output is the cache format `arbiter::FixtureBackend` already reads, so this
is a different source for an existing contract, not a new path.

    python3 run_tier.py <crops-dir> <out-dir> [model]
"""
import base64, json, os, re, sys, threading, time, urllib.error, urllib.request
from concurrent.futures import ThreadPoolExecutor

import google.auth.transport.requests
from google.oauth2 import service_account

KEY = os.environ["GOOGLE_APPLICATION_CREDENTIALS"]
PROJECT = os.environ.get("VERTEX_PROJECT") or json.load(open(KEY))["project_id"]
crops_dir, out_dir = sys.argv[1], sys.argv[2]
MODEL = sys.argv[3] if len(sys.argv) > 3 else "gemini-3-flash-preview"
os.makedirs(out_dir, exist_ok=True)

_creds = service_account.Credentials.from_service_account_file(
    KEY, scopes=["https://www.googleapis.com/auth/cloud-platform"])
_lock = threading.Lock()


def token():
    with _lock:
        if not _creds.valid:
            _creds.refresh(google.auth.transport.requests.Request())
        return _creds.token


TABLE_PROMPT = """This image is one table cropped from a document page.

Transcribe it as a single HTML table.

Requirements:
- Every row and every column that is printed, in the order printed.
- Transcribe values exactly as printed, including currency symbols, commas,
  decimals, percent signs, and parentheses for negatives.
- NEVER infer, compute, complete or correct a value. If a cell is blank in
  the image, emit an empty cell. If a value is unreadable, emit an empty cell.
- Merged cells: use rowspan / colspan.
- Use <th> for header cells, <td> for data cells.

Respond with the table markup only, starting with <table> and nothing else."""

# A routed region is whatever the text layer could not read: a chart, a
# scanned paragraph, or a table that was pasted in as a picture. The router
# does not know which, so the prompt must not assume. Told to transcribe a
# region "one item per line", the model flattened two image-tables into lines
# and their score went to zero -- the content was all there and the shape was
# gone. Let the image decide the form.
#
# The choice is per grid, not per image. Made whole-image, a crop holding a
# caption above a table answers "not a table" for the caption's sake and the
# grid is lost with it (bench 122). Each is transcribed in its own form, in
# the order printed.
REGION_PROMPT = """Transcribe what is printed in this image, exactly as printed,
in the order printed: top to bottom, then left to right.

Give each thing in the image the form that fits it.

- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column. It is still a table when it has no ruling lines, when
  it runs to many rows, and when it repeats its headers side by side to form
  a second pair of columns.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- A caption or note printed beside the table or chart becomes a plain text
  line, in the position it is printed.

In every case:
- This image is a crop of a larger page. Skip any line that runs off its left
  or right edge, and any line the top or bottom edge cuts through so that the
  letters are only part-height. That text belongs to something outside the
  crop and is transcribed elsewhere; transcribing it here duplicates it.
- Copy text exactly, including punctuation and decimal marks as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed in the image, return nothing at all.
- Do not describe the image and do not add commentary."""

# Prepended when the layout detector boxed a table inside this crop. It says
# what the image holds; the form rules above still decide how to write it.
TABLE_HINT = """This image contains a table: values arranged in rows and columns.
Transcribe that table as HTML markup, and any text printed outside it as plain
lines.

"""

# A link is the one piece of content a picture of a page cannot carry. The
# anchor is visible -- coloured, underlined -- and the address is not, so a
# model asked to transcribe the page sees that something is a link and has
# nothing to make it out of. It supplies one, and an invented URL is the worst
# kind of wrong answer: nothing downstream can tell it from a real one. Bench
# 108's contents page came back with `[About the Publisher](https://example.com)`.
#
# So the crop carries the addresses the file already states. `render-routed`
# reads them from the page's link annotations and writes them into the
# manifest, one entry per target with the wrapped fragments rejoined. The
# prompt then has something true to hold to, and the prohibition above covers
# whatever is left.
LINKS_HINT = """This image contains links. Their addresses are not printed on the
page, so you cannot read them from the image; they are given here:

{listing}

Where you transcribe one of those texts, write it as a Markdown link:
[text](address), using the address exactly as given. Any other text, however it
is styled, is not a link.

"""


def link_listing(links):
    """The manifest's links as prompt lines, or `None` for a crop with none."""
    lines = []
    for l in links or []:
        text, target = l.get("text", ""), l.get("target")
        if not text or target is None:
            continue
        # An internal jump has no address a Markdown reader can follow, so it
        # is named rather than given: it exists to stop the model inventing an
        # address for an anchor that never had one.
        if isinstance(target, dict):
            lines.append(f'  "{text}" links to page {target.get("page", 0) + 1} '
                         f"of this document -- write the text alone, with no link")
        else:
            lines.append(f'  "{text}" links to {target}')
    return "\n".join(lines) if lines else None


# A whole page is not a big region. A region is a fragment spliced back into a
# document that already has a shape; a page *is* the shape, so its reading has
# to carry headings and reading order, not just text. Two differences from
# REGION_PROMPT do that, and both were measured on bench 141:
#
#   * Markdown, so headings have somewhere to go. A page reading is spliced as
#     a single `text` block, and only a `title` label ever becomes a heading —
#     so a page could not produce one at all, whatever the model saw. 0.000 MHS.
#   * Columns named explicitly. "Top to bottom, then left to right" is right
#     for a region and describes a zip for a two-column page: the model
#     followed it exactly and interleaved 1,6,2,7,3,8. 0.618 NID.
FULL_PROMPT = """Transcribe this page exactly as printed, as Markdown.

Reading order follows the page's own layout. Where the page is laid out in
columns or panels, read each column to its end before starting the next; do
not read straight across the page.

Mark structure as the page marks it:

- A heading -- a line set apart by size, weight, colour, or its own banner --
  becomes a Markdown heading, `#` for the most prominent rank and `##`, `###`
  below it.
- A bulleted or numbered list becomes Markdown list items.
- A TABLE -- values arranged in rows and columns -- becomes HTML:
  <table><tr><td>...</td></tr></table>, one <tr> per printed row and one <td>
  per printed column.
- A chart, plot or diagram is NOT a table. Give its text only, one item per
  line: its title, its legend, every axis tick label, and both axis titles.
- Everything else is a paragraph.

In every case:
- Copy text exactly, including punctuation and dashes as printed.
- NEVER invent a value. Do not read a number off a bar's height or a wedge's
  angle: if it is not printed as text, it is not there.
- NEVER invent a link. Text may be coloured or underlined to show it is one,
  but the address is not printed and you cannot see it. Write the text alone.
- If nothing is printed on the page, return nothing at all.
- Do not describe the page and do not add commentary."""

URL = (f"https://aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/global"
       f"/publishers/google/models/{MODEL}:generateContent")


def kind_of(stem):
    tail = stem.rsplit("_", 1)[-1]
    if tail == "full":
        return "full"
    return "table" if tail.startswith("t") else "region"


def manifest_hints(crops):
    """Per-crop hints from `render-routed`: table flags and known links.

    Returns `(table stems, {stem: links})`. An older crops directory has no
    manifest, or a manifest without these fields; all of them read as "no
    hint", which is the pre-existing behaviour rather than an error.
    """
    path = os.path.join(crops, "manifest.jsonl")
    tables, links = set(), {}
    if not os.path.exists(path):
        return tables, links
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            png = rec.get("png")
            if not png:
                continue
            if rec.get("table"):
                tables.add(png[:-4])
            if rec.get("links"):
                links[png[:-4]] = rec["links"]
    return tables, links


HINTED, LINKS = manifest_hints(crops_dir)


def call(img_bytes, prompt):
    body = json.dumps({
        "contents": [{"role": "user", "parts": [
            {"inlineData": {"mimeType": "image/png",
                            "data": base64.b64encode(img_bytes).decode()}},
            {"text": prompt},
        ]}],
        "generationConfig": {
            "temperature": 0,
            "maxOutputTokens": 8000,
            "thinkingConfig": ({"thinkingLevel": "LOW"} if MODEL.startswith("gemini-3")
                               else {"thinkingBudget": 0}),
        },
    }).encode()
    for attempt in range(4):
        try:
            req = urllib.request.Request(URL, data=body, headers={
                "Authorization": f"Bearer {token()}", "Content-Type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=300))
            parts = (d.get("candidates") or [{}])[0].get("content", {}).get("parts", []) or []
            u = d.get("usageMetadata", {})
            return "".join(p.get("text", "") for p in parts), u
        except urllib.error.HTTPError as e:
            if e.code in (429, 500, 503) and attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            raise
        except Exception:
            if attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            raise
    return "", {}


# A reading that talks *about* the image is not a reading of it. Spliced, it
# enters the document as body text -- five benchmark documents carry a line
# reading "I did not find any text in this image." Empty is the right answer:
# the anchor drops and the deterministic output stands. Kept narrow on
# purpose, since this discards content: no table markup, at most two lines,
# and opening with a statement about the image rather than a transcription.
ABOUT_THE_IMAGE = re.compile(
    r"^(i'?m sorry|i cannot|i can'?t|i (am|'m) unable|i did not find"
    r"|i couldn'?t find|there (is|are) no|no text"
    r"|(the|this|that)?\s*(provided|given|supplied|attached)?\s*"
    r"(image|picture|crop)\b[^.]*\b(no|not)\b)",
    re.I,
)


def is_about_the_image(t):
    return (
        "<table" not in t.lower()
        and len(t) < 300
        and len(t.splitlines()) <= 2
        and bool(ABOUT_THE_IMAGE.match(t))
    )


def unwrap_single_row(t):
    """A one-row table is not a table.

    The rows-and-columns rule fires on any set of items in a line -- three
    picture captions side by side came back as a single `<tr>` (bench 176).
    One row carries no structure to be right or wrong about, so the markup is
    only a claim, and stripping it back to lines costs nothing where the
    reading was right and stops it asserting a table where it was not.
    """
    lower = t.lower()
    i = lower.find("<table")
    if i < 0 or lower.count("<tr") != 1:
        return t
    j = lower.rfind("</table>")
    if j < i:
        return t
    cells = re.findall(r"<t[dh][^>]*>(.*?)</t[dh]>", t[i:j], re.S | re.I)
    if not cells:
        return t
    lines = [re.sub(r"<[^>]+>", "", c).strip() for c in cells]
    return (t[:i] + "\n".join(x for x in lines if x) + t[j + 8:]).strip()


def clean(txt, kind):
    t = (txt or "").strip()
    if t.startswith("```"):
        t = t.split("\n", 1)[-1].rsplit("```", 1)[0].strip()
    if is_about_the_image(t):
        return ""
    if kind == "table":
        # A table crop that came back as anything else is not usable.
        i, j = t.find("<table"), t.rfind("</table>")
        return t[i:j + 8] if i >= 0 and j > i else ""
    # A region is spliced whole, so keep what surrounds the markup: the crop
    # is a slice of page, and its caption is content the deterministic pass
    # never had. Trimming to the <table> span discarded it.
    return unwrap_single_row(t)


def one(png):
    stem = png[:-4]
    dst = os.path.join(out_dir, stem + ".json")
    if os.path.exists(dst):
        return None
    kind = kind_of(stem)
    hinted = stem in HINTED
    if kind == "table":
        prompt = TABLE_PROMPT
    elif kind == "full":
        prompt = FULL_PROMPT
    else:
        prompt = (TABLE_HINT if hinted else "") + REGION_PROMPT
    # A table is transcribed as HTML markup, which has no place to put a
    # Markdown link; the other two carry prose.
    listing = link_listing(LINKS.get(stem)) if kind != "table" else None
    if listing:
        prompt = LINKS_HINT.format(listing=listing) + prompt
    t0 = time.time()
    try:
        raw, usage = call(open(os.path.join(crops_dir, png), "rb").read(), prompt)
    except Exception as e:
        print(f"  {stem} FAILED {type(e).__name__} {str(e)[:70]}", flush=True)
        return None
    content = clean(raw, kind)
    label = "table" if content.lstrip().startswith("<table") else "text"
    json.dump([{"parsing_res_list": [
        {"block_label": label, "block_content": content, "block_order": 0}]}],
        open(dst, "w"), ensure_ascii=False)
    rec = {"s": round(time.time() - t0, 2),
           "in": usage.get("promptTokenCount"),
           "out": usage.get("candidatesTokenCount"),
           "think": usage.get("thoughtsTokenCount", 0),
           "kind": kind, "hinted": hinted, "links": bool(listing),
           "chars": len(content)}
    tag = f"{kind}+hint" if hinted else kind
    print(f"  {stem} [{tag}] {rec['s']}s chars={rec['chars']}", flush=True)
    return stem, rec


pngs = sorted(f for f in os.listdir(crops_dir) if f.endswith(".png"))
print(f"{len(pngs)} crops -> {MODEL}")
timings = {}
tf = os.path.join(out_dir, "_timings.json")
if os.path.exists(tf):
    timings = json.load(open(tf))
with ThreadPoolExecutor(max_workers=6) as ex:
    for r in ex.map(one, pngs):
        if r:
            timings[r[0]] = r[1]
json.dump(timings, open(tf, "w"), indent=1)
print("DONE", len(timings), "read")
