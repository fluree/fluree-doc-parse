"""Shared paths and the glyph-layer primitives the rubric is built on.

Every measurement here is anchored to the PDF's own glyph layer rather than
to our structured output. On flagged pages our reading is the thing under
test, so using it as ground truth would measure our defect and call it the
model's.
"""
import json, os, re, subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FDOC = os.environ.get("FLUREE_DOC_BINARY", os.path.join(ROOT, "target", "release", "fdoc"))
CORPUS = os.environ.get("LLM_TIER_CORPUS", os.path.join(ROOT, "eval", "corpus-gaps"))
WORK = os.environ.get("LLM_TIER_WORK", os.path.join(ROOT, "eval", "llm-tier", "work"))

REGIONS = os.path.join(WORK, "regions.json")
CROPS = os.path.join(WORK, "crops")
PAGELINES = os.path.join(WORK, "pagelines.json")
REGIONLINES = os.path.join(WORK, "regionlines.json")

GLYPH = re.compile(r"^\s*#\d+\s+([-\d.]+),\s*([-\d.]+)\s+sz=\s*([\d.]+).*?\"(.*)\"\s*$")


def fdoc(*args):
    return subprocess.run([FDOC, *args], capture_output=True, text=True).stdout


def page_glyphs(doc, page):
    """(y, x, size, char) for every glyph on a page, in draw order."""
    out = []
    for ln in fdoc("dev", "glyphs", os.path.join(CORPUS, doc + ".pdf"), str(page)).splitlines():
        m = GLYPH.match(ln)
        if m and m.group(4).strip():
            out.append((float(m.group(2)), float(m.group(1)), float(m.group(3)), m.group(4)))
    return out


def lines_of(glyphs):
    """Group glyphs into baselines, spacing only where the page leaves a gap.

    Joining without gaps would fuse adjacent column values into one token;
    joining every glyph with a space would split thousands separators. The
    gap test reproduces what the page shows, which is what a reader claiming
    to have read it must match.
    """
    glyphs = sorted(glyphs, key=lambda g: (round(g[0], 1), g[1]))
    lines, band, cy = [], [], None

    def flush(b):
        b.sort(key=lambda g: g[1])
        out, prev = [], None
        for _, x, sz, ch in b:
            if prev is not None and x - prev > sz * 0.28:
                out.append(" ")
            out.append(ch)
            prev = x + sz * 0.5
        return "".join(out)

    for g in glyphs:
        if cy is None or abs(g[0] - cy) <= max(1.5, g[2] * 0.3):
            band.append(g)
            cy = g[0] if cy is None else cy
        else:
            lines.append(flush(band))
            band, cy = [g], g[0]
    if band:
        lines.append(flush(band))
    return lines


def key(doc, page):
    return f"{doc}\x1f{page}"


def unkey(k):
    d, p = k.split("\x1f")
    return d, int(p)


def load(path):
    return json.load(open(path)) if os.path.exists(path) else {}


def save(obj, path):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    json.dump(obj, open(path, "w"), ensure_ascii=False)
