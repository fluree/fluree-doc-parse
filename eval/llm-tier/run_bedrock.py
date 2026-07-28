"""Read a table crop with a multimodal model on Bedrock.

The crop is the unit deliberately: the strongest crop-unit reader we
measured scored 0.925 TEDS on crops and lost 0-20 to docling on whole
pages, so what a vision model is shown matters more than which one it is.

The prompt states the output contract exactly and forbids inference. For a
financial table the dangerous failure is not a missing number but a
plausible one -- a model that completes a total it cannot read produces
output that looks right and is not. Whether the instruction holds is
measured separately, not assumed.
"""
import json, os, re, sys, time, boto3

model_id, out_dir, crops = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(out_dir, exist_ok=True)
br = boto3.Session(profile_name="fluree-dev", region_name="us-east-1").client("bedrock-runtime")

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

def extract_json(txt):
    t = txt.strip()
    t = re.sub(r"^```(?:json)?\s*", "", t)
    t = re.sub(r"\s*```$", "", t)
    i, j = t.find("{"), t.rfind("}")
    return json.loads(t[i:j+1]) if i >= 0 and j > i else {}

timings = {}
for png in sorted(os.listdir(crops)):
    if not png.endswith(".png"):
        continue
    stem = png[:-4]
    dst = os.path.join(out_dir, stem + ".json")
    if os.path.exists(dst):
        continue
    img = open(os.path.join(crops, png), "rb").read()
    t0 = time.time()
    try:
        r = br.converse(
            modelId=model_id,
            messages=[{"role": "user", "content": [
                {"image": {"format": "png", "source": {"bytes": img}}},
                {"text": PROMPT},
            ]}],
            inferenceConfig={"maxTokens": 8000, "temperature": 0},
        )
        txt = "".join(c.get("text", "") for c in r["output"]["message"]["content"])
        u = r.get("usage", {})
        html = extract_json(txt).get("table", "")
        if not isinstance(html, str):
            html = ""
        json.dump({"html": html}, open(dst, "w"), ensure_ascii=False)
        timings[stem] = {"s": round(time.time() - t0, 2),
                         "in": u.get("inputTokens"), "out": u.get("outputTokens")}
        print(f"  {stem} {time.time()-t0:.1f}s out={u.get('outputTokens')}", flush=True)
    except Exception as e:
        print(f"  {stem} FAILED {type(e).__name__}: {str(e)[:70]}", flush=True)
json.dump(timings, open(os.path.join(out_dir, "_timings.json"), "w"), indent=1)
print("DONE")
