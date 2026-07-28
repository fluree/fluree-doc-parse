"""Read table crops with Gemini on Vertex AI.

Same crops, same prompt, same output contract as run_llm.py (Bedrock), so
the two engines are judged by one rubric. The only differences are the
transport and the thinking budget, which is the efficiency knob worth
measuring separately: a flash model that thinks costs several times a
flash model that does not, and whether the thinking buys accuracy on a
transcription task is exactly the open question.
"""
import base64, json, os, re, sys, threading, time, urllib.error, urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import google.auth.transport.requests
from google.oauth2 import service_account

KEY = os.environ["GOOGLE_APPLICATION_CREDENTIALS"]
PROJECT = os.environ.get("VERTEX_PROJECT") or json.load(open(KEY))["project_id"]

model_id = sys.argv[1]
out_dir = sys.argv[2]
crops = sys.argv[3]
# thinking budget: -1 dynamic (default), 0 off, N tokens
think = int(sys.argv[4]) if len(sys.argv) > 4 else -1
os.makedirs(out_dir, exist_ok=True)

_creds = service_account.Credentials.from_service_account_file(
    KEY, scopes=["https://www.googleapis.com/auth/cloud-platform"])
_lock = threading.Lock()


def token():
    with _lock:
        if not _creds.valid:
            _creds.refresh(google.auth.transport.requests.Request())
        return _creds.token


PROMPT = """This image is one table cropped from a document page.

Transcribe it as a single HTML table.

Requirements:
- Every row and every column that is printed, in the order printed.
- Transcribe values exactly as printed, including currency symbols, commas,
  decimals, percent signs, and parentheses for negatives.
- NEVER infer, compute, complete or correct a value. If a cell is blank in
  the image, emit an empty cell. If a value is unreadable, emit an empty cell.
- Merged cells: use rowspan / colspan. Do not repeat a merged value into the
  cells it spans.
- Use <th> for header cells, <td> for data cells.
- Do not include the table's caption, footnotes, or page furniture.

Respond with only this JSON, no explanation and no markdown fence:
{"table": "<table>...</table>"}"""

URL = (f"https://aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/global"
       f"/publishers/google/models/{model_id}:generateContent")


def extract_json(txt):
    t = (txt or "").strip()
    t = re.sub(r"^```(?:json)?\s*", "", t)
    t = re.sub(r"\s*```$", "", t)
    i, j = t.find("{"), t.rfind("}")
    if i < 0 or j <= i:
        return {}
    try:
        return json.loads(t[i:j + 1])
    except Exception:
        # a raw <table> answer is still usable
        k, l = t.find("<table"), t.rfind("</table>")
        return {"table": t[k:l + 8]} if k >= 0 and l > k else {}


def gen_config():
    cfg = {"temperature": 0, "maxOutputTokens": 16000}
    if think >= 0:
        if model_id.startswith("gemini-3"):
            cfg["thinkingConfig"] = {"thinkingLevel": "LOW" if think == 0 else "HIGH"}
        else:
            cfg["thinkingConfig"] = {"thinkingBudget": think}
    return cfg


def one(png):
    stem = png[:-4]
    dst = os.path.join(out_dir, stem + ".json")
    if os.path.exists(dst):
        return None
    img = base64.b64encode(open(os.path.join(crops, png), "rb").read()).decode()
    body = json.dumps({
        "contents": [{"role": "user", "parts": [
            {"inlineData": {"mimeType": "image/png", "data": img}},
            {"text": PROMPT},
        ]}],
        "generationConfig": gen_config(),
    }).encode()
    t0 = time.time()
    for attempt in range(4):
        try:
            req = urllib.request.Request(URL, data=body, headers={
                "Authorization": f"Bearer {token()}", "Content-Type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=300))
            break
        except urllib.error.HTTPError as e:
            msg = e.read().decode()[:150].replace("\n", " ")
            if e.code in (429, 500, 503) and attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            print(f"  {stem} HTTP {e.code} {msg}", flush=True)
            return None
        except Exception as e:
            if attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            print(f"  {stem} {type(e).__name__} {str(e)[:80]}", flush=True)
            return None
    else:
        return None

    cands = d.get("candidates") or [{}]
    parts = cands[0].get("content", {}).get("parts", []) or []
    txt = "".join(p.get("text", "") for p in parts)
    html = extract_json(txt).get("table", "")
    if not isinstance(html, str):
        html = ""
    u = d.get("usageMetadata", {})
    json.dump({"html": html}, open(dst, "w"), ensure_ascii=False)
    rec = {"s": round(time.time() - t0, 2),
           "in": u.get("promptTokenCount"),
           "out": u.get("candidatesTokenCount"),
           "think": u.get("thoughtsTokenCount", 0),
           "finish": cands[0].get("finishReason")}
    print(f"  {stem} {rec['s']}s out={rec['out']} think={rec['think']} "
          f"{'EMPTY' if not html else ''}", flush=True)
    return stem, rec


pngs = sorted(f for f in os.listdir(crops) if f.endswith(".png"))
timings = {}
tf = os.path.join(out_dir, "_timings.json")
if os.path.exists(tf):
    timings = json.load(open(tf))
with ThreadPoolExecutor(max_workers=6) as ex:
    for r in ex.map(one, pngs):
        if r:
            timings[r[0]] = r[1]
json.dump(timings, open(tf, "w"), indent=1)
print("DONE", model_id, len(timings))
