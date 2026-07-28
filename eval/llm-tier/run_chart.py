"""Read a chart crop with Gemini, as a figure rather than a table.

A chart's failure mode is not a missing value, it is a value attached to the
wrong label. The deterministic reader recovers every glyph of J&J's employee
donut and still cannot say which percentage belongs to which region, because
the association is carried by colour and wedge geometry, not by position in
the text stream.

So the ask here is narrow and checkable: the series, each label paired with
its value, and nothing invented. Labels and values are both printed on the
page, so both halves can still be verified against the glyph layer -- only
the pairing cannot, which is exactly why it is worth asking a model for and
worth marking as weaker evidence when it arrives.

    python3 run_chart.py <model> <out-dir> <crops-dir> [thinking-budget]
"""
import base64, json, os, re, sys, threading, time, urllib.error, urllib.request

import google.auth.transport.requests
from google.oauth2 import service_account

KEY = os.environ["GOOGLE_APPLICATION_CREDENTIALS"]
PROJECT = os.environ.get("VERTEX_PROJECT") or json.load(open(KEY))["project_id"]

model_id, out_dir, crops = sys.argv[1], sys.argv[2], sys.argv[3]
think = int(sys.argv[4]) if len(sys.argv) > 4 else 0
os.makedirs(out_dir, exist_ok=True)

_creds = service_account.Credentials.from_service_account_file(
    KEY, scopes=["https://www.googleapis.com/auth/cloud-platform"])
_lock = threading.Lock()


def token():
    with _lock:
        if not _creds.valid:
            _creds.refresh(google.auth.transport.requests.Request())
        return _creds.token


PROMPT = """This image is one chart or figure cropped from a document page.

Report what it shows, as JSON only, in exactly this shape:

{"kind": "<pie|donut|bar|line|area|timeline|other>",
 "title": "<the chart's own title, or empty>",
 "series": [{"label": "<printed label>", "value": "<printed value>",
             "category": "<axis/group label if the chart has one, else empty>"}],
 "unit": "<percent|currency|count|other, from the chart's own labelling>",
 "notes": "<anything printed that a reader needs, e.g. 'dollars in billions'>"}

Rules:
- Pair each label with the value that belongs to it *in the drawing* -- the
  wedge, bar or point it labels -- not by reading order.
- Copy labels and values exactly as printed, including %, $, commas, decimals.
- NEVER infer, compute, complete or correct a value. If a value is not
  printed, use "" for it. Do not derive one from a wedge angle or bar height.
- If the same label appears for several categories (e.g. two years), emit one
  entry per category.
- Report only what is printed in this image."""

URL = (f"https://aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/global"
       f"/publishers/google/models/{model_id}:generateContent")


def extract(txt):
    t = re.sub(r"\s*```$", "", re.sub(r"^```(?:json)?\s*", "", (txt or "").strip()))
    i, j = t.find("{"), t.rfind("}")
    try:
        return json.loads(t[i:j + 1]) if i >= 0 and j > i else {}
    except Exception:
        return {}


def cfg():
    c = {"temperature": 0, "maxOutputTokens": 8000}
    if model_id.startswith("gemini-3"):
        c["thinkingConfig"] = {"thinkingLevel": "LOW" if think == 0 else "HIGH"}
    else:
        c["thinkingConfig"] = {"thinkingBudget": think}
    return c


for png in sorted(f for f in os.listdir(crops) if f.endswith(".png")):
    stem = png[:-4]
    dst = os.path.join(out_dir, stem + ".json")
    if os.path.exists(dst):
        continue
    img = base64.b64encode(open(os.path.join(crops, png), "rb").read()).decode()
    body = json.dumps({
        "contents": [{"role": "user", "parts": [
            {"inlineData": {"mimeType": "image/png", "data": img}},
            {"text": PROMPT},
        ]}],
        "generationConfig": cfg(),
    }).encode()
    t0 = time.time()
    try:
        req = urllib.request.Request(URL, data=body, headers={
            "Authorization": f"Bearer {token()}", "Content-Type": "application/json"})
        d = json.load(urllib.request.urlopen(req, timeout=300))
    except urllib.error.HTTPError as e:
        print(f"  {stem} HTTP {e.code} {e.read().decode()[:120]}", flush=True)
        continue
    parts = (d.get("candidates") or [{}])[0].get("content", {}).get("parts", []) or []
    got = extract("".join(p.get("text", "") for p in parts))
    u = d.get("usageMetadata", {})
    json.dump(got, open(dst, "w"), ensure_ascii=False, indent=1)
    print(f"  {stem} {time.time()-t0:.1f}s in={u.get('promptTokenCount')} "
          f"out={u.get('candidatesTokenCount')} series={len(got.get('series', []))}",
          flush=True)
print("DONE")
