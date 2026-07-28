"""Rank engine runs over the flagged tables, by one rubric.

    python3 score.py <run-dir> [run-dir ...]

Each run directory holds one `<region-name>.json` per crop, `{"html": "..."}`,
plus an optional `_timings.json` for cost and latency. Our own reading is
always scored alongside as the control.

Runs are compared on the crops *every* run returned a table for. A run that
answered nothing on the three hardest tables would otherwise be rewarded for
skipping them; coverage is reported separately, in the `empty` column, where
a skip is visible as a skip. The hard cases are where engines diverge, so
`--only <prefix>` exists to look at them directly rather than through an
average that has excluded them.
"""
import json, os, sys

from common import PAGELINES, REGIONLINES, REGIONS, key, load
from rubric import rows_of_html, score, spans

# $ per 1M tokens (input, output); thinking bills at the output rate.
# cloud.google.com/vertex-ai/generative-ai/pricing, aws.amazon.com/bedrock/pricing
PRICE = {
    "gemini-2.5-flash-lite": (0.10, 0.40),
    "gemini-2.5-flash": (0.30, 2.50),
    "gemini-2.5-pro": (1.25, 10.00),
    "gemini-3-flash": (1.50, 9.00),
    "nova-pro": (0.80, 3.20),
}


def price_for(name):
    for k, v in PRICE.items():
        if k in name:
            return v
    return None


def main(argv):
    only = None
    runs = []
    it = iter(argv)
    for a in it:
        if a == "--only":
            only = next(it)
        else:
            runs.append(a)

    regions = load(REGIONS)
    plines = {unk: v for unk, v in load(PAGELINES).items()}
    rlines = load(REGIONLINES)
    if not regions or not plines:
        sys.exit("run regions.py, crops.py and groundtruth.py first")
    if only:
        regions = [r for r in regions if r["name"].startswith(only)]

    def read(run, name):
        f = os.path.join(run, name + ".json")
        if not os.path.exists(f):
            return None
        try:
            return json.load(open(f)).get("html", "")
        except Exception:
            return None

    common = [r for r in regions
              if all(rows_of_html(read(d, r["name"]) or "") for d in runs)] or regions
    print(f"scored on {len(common)} of {len(regions)} tables"
          f"{' (' + only + ')' if only else ''}"
          f" — those every run returned a table for\n")

    fmt = "{:<24} {:>4} {:>5} {:>6} {:>5} {:>6} {:>7} {:>7} {:>8}"
    hdr = fmt.format("run", "read", "none", "rows", "cram", "spans", "recall", "fab%", "cost$")
    print(hdr)
    print("-" * len(hdr))

    def total(get_rows, get_html=None):
        acc = dict.fromkeys(("rows", "crammed", "values", "fabricated", "hit", "truth"), 0)
        acc["spans"] = 0
        n = 0
        for r in common:
            rows = get_rows(r)
            if rows is None:
                continue
            n += 1
            s = score(rows, plines[key(r["doc"], r["page"])], rlines.get(r["name"], []))
            for k in ("rows", "crammed", "values", "fabricated", "hit", "truth"):
                acc[k] += s[k]
            if get_html:
                acc["spans"] += spans(get_html(r) or "")
        return n, acc

    def show(label, n, a, gone, cost, extra=""):
        print(fmt.format(label, n, gone, a["rows"], a["crammed"], a["spans"],
                         f"{a['hit']/max(1,a['truth'])*100:.1f}%",
                         f"{a['fabricated']/max(1,a['values'])*100:.1f}",
                         cost))
        if extra:
            print(f"{'':24} {extra}")

    n, a = total(lambda r: r["cells"] or [])
    show("ours (deterministic)", n, a, 0, "—")

    for run in runs:
        n, a = total(lambda r: rows_of_html(read(run, r["name"]) or "") or None,
                     lambda r: read(run, r["name"]))
        gone = sum(1 for r in regions if not rows_of_html(read(run, r["name"]) or ""))
        cost, extra = "n/a", ""
        tf = os.path.join(run, "_timings.json")
        if os.path.exists(tf):
            tm = json.load(open(tf))
            pin_pout = price_for(os.path.basename(run.rstrip("/")))
            tin = sum(v.get("in") or 0 for v in tm.values())
            tout = sum((v.get("out") or 0) + (v.get("think") or 0) for v in tm.values())
            if pin_pout:
                cost = f"{tin/1e6*pin_pout[0] + tout/1e6*pin_pout[1]:.2f}"
            secs = sorted(v.get("s", 0) for v in tm.values())
            extra = (f"tokens in {tin} out+think {tout} "
                     f"median {secs[len(secs)//2]:.1f}s" if secs else "")
        show(os.path.basename(run.rstrip("/")), n, a, gone, cost, extra)


if __name__ == "__main__":
    main(sys.argv[1:])
