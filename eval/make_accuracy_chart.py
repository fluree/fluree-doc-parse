#!/usr/bin/env python3
"""Render the accuracy-against-speed chart for the README.

  python3 eval/make_accuracy_chart.py
  # -> docs/assets/accuracy-vs-speed.svg

One point per engine on opendataloader-bench: the overall score against
seconds per page, on a log axis. Every document in the corpus is one page, so
the harness's seconds per document are seconds per page. fluree-doc-parse's
three tiers are joined in order, the way a document walks up them.

The score axis starts at Y_LO so the leading engines have room to spread out.
Engines scoring below it are listed in a note in the plot's empty corner, so
every engine still appears.

Hand-rolled SVG with no dependencies, so it renders as a plain <img> on GitHub.
Update ENGINES when the scores change, and re-run.
"""
import math
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs", "assets", "accuracy-vs-speed.svg")

MEASURED = "2026-10-07"

# (label, overall, seconds per page). Other engines' timings are the harness's
# own. Ours: tier 1 is the warm median, as the harness spawns one process per
# document; tiers 2 and 3 are the corpus average including model time, since
# the harness only times the replay of cached model output.
TIERS = [
    ("Tier 1 · deterministic", 0.893524, 0.009),
    ("Tier 2 · layout detector", 0.901298, 0.2),
    ("Tier 3 · cascade", 0.934296, 1.5),
]
ENGINES = [
    ("opendataloader-hybrid", 0.906572, 0.4627),
    ("nutrient", 0.885067, 0.00834),
    ("docling", 0.881679, 0.7622),
    ("opendataloader-hybrid-hydrogen", 0.876816, 5.068),
    ("pdf-inspector", 0.875348, 0.006),
    ("marker", 0.860836, 53.93),
    ("opendataloader-hybrid-helium", 0.845058, 8.62),
    ("unstructured-hires", 0.841377, 3.008),
    ("edgeparse", 0.836959, 0.0363),
    ("opendataloader", 0.831209, 0.0150),
    ("mineru", 0.831135, 5.962),
    ("pymupdf4llm", 0.731621, 0.0909),
    ("unstructured", 0.685777, 0.0773),
    ("markitdown", 0.588504, 0.1140),
    ("liteparse", 0.575604, 1.0606),
]
# Label placement per engine where the default (right of the point) collides:
# (dx, dy, anchor).
PLACE = {
    "nutrient": (-9, 4, "end"),
    "pdf-inspector": (-9, 8, "end"),
    "opendataloader": (-9, 10, "end"),
    "edgeparse": (9, -3, "start"),
    "opendataloader-hybrid": (9, 5, "start"),
    "docling": (9, 12, "start"),
    "unstructured-hires": (-9, 4, "end"),
    "opendataloader-hybrid-hydrogen": (9, -6, "start"),
    "opendataloader-hybrid-helium": (9, 13, "start"),
    "mineru": (-9, 12, "end"),
    "marker": (-9, 4, "end"),
}
TIER_PLACE = [(0, -14, "middle"), (-6, 22, "end"), (12, 4, "start")]

FLUREE = "#0d9488"  # teal, as in the Fluree benchmark charts
OTHER = "#64748b"
INK = "#1e293b"
MUTED = "#64748b"
GRID = "#e2e8f0"
FONT = "-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif"

W, H = 880, 500
LEFT, RIGHT, TOP, BOTTOM = 64, 856, 104, 430
X_LO, X_HI = 0.001, 100.0
Y_LO, Y_HI = 0.65, 0.95


def x(s):
    f = (math.log10(s) - math.log10(X_LO)) / (math.log10(X_HI) - math.log10(X_LO))
    return LEFT + f * (RIGHT - LEFT)


def y(v):
    return BOTTOM - (v - Y_LO) / (Y_HI - Y_LO) * (BOTTOM - TOP)


def esc(t):
    return t.replace("&", "&amp;").replace("<", "&lt;")


def main():
    s = [
        f"<svg xmlns='http://www.w3.org/2000/svg' width='{W}' height='{H}' viewBox='0 0 {W} {H}' "
        f"font-family='{FONT}'>",
        f"<rect width='{W}' height='{H}' fill='white'/>",
        f"<text x='20' y='28' font-size='17' font-weight='700' fill='{INK}'>"
        "Accuracy against speed · opendataloader-bench</text>",
        f"<text x='20' y='48' font-size='12' fill='{MUTED}'>200 single-page PDFs · overall score, the "
        f"mean of reading order, table and heading scores · {len(ENGINES) + 2} engines · {MEASURED}</text>",
    ]
    # Legend: two series, so a legend as well as direct labels.
    ly = 74
    s.append(f"<circle cx='26' cy='{ly - 4}' r='5.5' fill='{FLUREE}'/>")
    s.append(f"<text x='37' y='{ly}' font-size='12' font-weight='700' fill='{INK}'>fluree-doc-parse, by tier</text>")
    s.append(f"<circle cx='206' cy='{ly - 4}' r='4.5' fill='{OTHER}'/>")
    s.append(f"<text x='216' y='{ly}' font-size='12' fill='{INK}'>other engines</text>")
    s.append(f"<text x='{RIGHT}' y='{ly}' font-size='12' font-weight='700' fill='{INK}' text-anchor='end'>"
             "higher is more accurate · left is faster</text>")

    # Grid and axes.
    for v in (0.70, 0.75, 0.80, 0.85, 0.90, 0.95):
        yy = y(v)
        s.append(f"<line x1='{LEFT}' y1='{yy:.1f}' x2='{RIGHT}' y2='{yy:.1f}' stroke='{GRID}'/>")
        s.append(f"<text x='{LEFT - 8}' y='{yy + 4:.1f}' font-size='11' fill='{MUTED}' text-anchor='end'>{v:.2f}</text>")
    for t, label in ((0.001, "1 ms"), (0.01, "10 ms"), (0.1, "100 ms"), (1, "1 s"), (10, "10 s"), (100, "100 s")):
        xx = x(t)
        s.append(f"<line x1='{xx:.1f}' y1='{TOP}' x2='{xx:.1f}' y2='{BOTTOM}' stroke='{GRID}'/>")
        s.append(f"<text x='{xx:.1f}' y='{BOTTOM + 17}' font-size='11' fill='{MUTED}' text-anchor='middle'>{label}</text>")
    s.append(f"<line x1='{LEFT}' y1='{BOTTOM}' x2='{RIGHT}' y2='{BOTTOM}' stroke='{MUTED}' stroke-opacity='0.5'/>")
    s.append(f"<text x='{(LEFT + RIGHT) / 2:.0f}' y='{BOTTOM + 36}' font-size='11.5' fill='{INK}' "
             "text-anchor='middle'>seconds per page (log scale)</text>")
    s.append(f"<text x='16' y='{(TOP + BOTTOM) / 2:.0f}' font-size='11.5' fill='{INK}' text-anchor='middle' "
             f"transform='rotate(-90 16 {(TOP + BOTTOM) / 2:.0f})'>overall score</text>")

    # Other engines, under ours.
    for name, score, secs in (e for e in ENGINES if e[1] >= Y_LO):
        px, py = x(secs), y(score)
        s.append(f"<circle cx='{px:.1f}' cy='{py:.1f}' r='4.5' fill='{OTHER}' stroke='white' stroke-width='2'/>")
        dx, dy, anchor = PLACE.get(name, (9, 4, "start"))
        s.append(f"<text x='{px + dx:.1f}' y='{py + dy:.1f}' font-size='11' fill='{MUTED}' "
                 f"text-anchor='{anchor}'>{esc(name)}</text>")

    # Our tiers, joined in the order a document walks them.
    pts = [(x(t), y(v)) for _, v, t in TIERS]
    path = " ".join(f"{'M' if i == 0 else 'L'}{px:.1f} {py:.1f}" for i, (px, py) in enumerate(pts))
    s.append(f"<path d='{path}' fill='none' stroke='{FLUREE}' stroke-width='2' stroke-linejoin='round'/>")
    for (label, score, secs), (px, py), (dx, dy, anchor) in zip(TIERS, pts, TIER_PLACE):
        s.append(f"<circle cx='{px:.1f}' cy='{py:.1f}' r='6' fill='{FLUREE}' stroke='white' stroke-width='2'/>")
        s.append(f"<text x='{px + dx:.1f}' y='{py + dy:.1f}' font-size='12' font-weight='700' fill='{INK}' "
                 f"text-anchor='{anchor}'>{esc(label)} <tspan font-weight='400' fill='{MUTED}'>{score:.3f}</tspan></text>")

    # Engines below the score axis, in a note in the bottom-right corner.
    below = sorted((e for e in ENGINES if e[1] < Y_LO), key=lambda e: -e[1])
    if below:
        row, pad, bw = 17, 10, 238
        bh = pad * 2 + row * (len(below) + 1) - 4
        bx, by = RIGHT - bw - 10, BOTTOM - bh - 10
        s.append(f"<rect x='{bx}' y='{by}' width='{bw}' height='{bh}' rx='8' fill='#f8fafc' "
                 f"stroke='{GRID}'/>")
        s.append(f"<text x='{bx + pad}' y='{by + pad + 10}' font-size='11' font-weight='700' "
                 f"fill='{INK}'>Below {Y_LO:.2f}, off the scale</text>")
        for i, (name, score, secs) in enumerate(below):
            ty = by + pad + 10 + row * (i + 1)
            t = f"{secs * 1000:.0f} ms" if secs < 1 else f"{secs:.2f} s"
            s.append(f"<circle cx='{bx + pad + 4}' cy='{ty - 4}' r='4' fill='{OTHER}'/>")
            s.append(f"<text x='{bx + pad + 14}' y='{ty}' font-size='11' fill='{MUTED}'>{esc(name)}</text>")
            s.append(f"<text x='{bx + bw - pad}' y='{ty}' font-size='11' fill='{MUTED}' text-anchor='end'>"
                     f"{score:.3f} · {t}</text>")

    s.append(f"<text x='20' y='{H - 14}' font-size='10.5' fill='{MUTED}'>Other engines' times are the "
             "benchmark's own. Tier 1 is a warm median; tiers 2 and 3 include model time, averaged over the corpus.</text>")
    s.append("</svg>")
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w") as f:
        f.write("\n".join(s) + "\n")
    print(OUT)


if __name__ == "__main__":
    main()
