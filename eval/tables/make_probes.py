# /// script
# requires-python = ">=3.11"
# dependencies = ["reportlab>=4"]
# ///
"""Write the hand-drawn PDF probes for tables without vertical rules.

Each probe is one layout the real-document audit found unread (Stage 27,
package 4b, in the fluree-nakaya lab record), drawn here with the geometry
the published pages use, so the fixture holds no third-party page:

* `probe-statement`: a financial statement. Labels with dot leaders, `$`
  set apart from its figure, a short rule under each column's figures and a
  double rule under the total, every rule a subpath of one stroked path, a
  banner (`Fiscal`) with a rule under the columns it names, a unit line
  centred under the years, a section label and a label wrapped onto the line
  that carries its figures.
* `probe-booktabs`: a journal table between three full-width rules, a stub
  centred across two header lines, a banner over year ranges with a rule
  under them, and a column label wrapped over two lines.
* `probe-statement-far`: labels at the margin and figures at the far side
  of the page, no rules and no leaders.

    uv run eval/tables/make_probes.py
"""

import json
from pathlib import Path

from reportlab.pdfbase.pdfmetrics import stringWidth
from reportlab.pdfgen import canvas

OUT = Path(__file__).resolve().parent / "fixtures"
FONT, BOLD = "Helvetica", "Helvetica-Bold"


def text(c, x, y, s, size=9, font=FONT):
    c.setFont(font, size)
    c.drawString(x, y, s)


def right(c, x, y, s, size=9, font=FONT):
    c.setFont(font, size)
    c.drawRightString(x, y, s)


def centre(c, x, y, s, size=9, font=FONT):
    c.setFont(font, size)
    c.drawCentredString(x, y, s)


def leaders(c, x0, x1, y, size=9):
    """Dots from x0 to x1, set a dot and a space apart."""
    step = stringWidth(". ", FONT, size)
    x = x0
    c.setFont(FONT, size)
    while x + step < x1:
        c.drawString(x, y, ".")
        x += step


def paragraph(c, x, y, lines, size=10):
    for i, s in enumerate(lines):
        text(c, x, y - i * size * 1.25, s, size)


def statement():
    path = OUT / "probe-statement.pdf"
    c = canvas.Canvas(str(path), pagesize=(612, 792))
    paragraph(c, 72, 720, ["The following table sets forth the denominators of the basic and diluted",
                           "earnings per share computations:"])
    cols = [430, 470, 510]  # right edges of the three figure columns
    w = 32
    rules = []  # (x0, x1, y): every rule goes into one stroked path
    centre(c, (cols[0] - w + cols[2]) / 2, 660, "Fiscal", 8, BOLD)
    rules.append((cols[0] - w, cols[2], 656))
    for x, year in zip(cols, ["2012", "2011", "2010"]):
        right(c, x, 646, year, 8, BOLD)
        rules.append((x - w, x, 642))
    centre(c, (cols[0] - w + cols[2]) / 2, 632, "(in millions)", 8, BOLD)
    text(c, 100, 616, "Weighted-average shares outstanding:")
    body = [
        ("Basic", ["426", "438", "453"], None),
        ("Dilutive share options and restricted share awards", ["4", "5", "4"], "single"),
        ("Diluted", ["430", "443", "457"], "double"),
    ]
    y = 602
    for label, figs, under in body:
        text(c, 112, y, label)
        leaders(c, 116 + stringWidth(label, FONT, 9), cols[0] - w - 6, y)
        for x, f in zip(cols, figs):
            right(c, x, y, f)
        if under == "single":
            rules.extend((x - w + 4, x, y - 4) for x in cols)
        if under == "double":
            rules.extend((x - w + 4, x, y - 4) for x in cols)
            rules.extend((x - w + 4, x, y - 6) for x in cols)
        y -= 16
    # A label wrapped onto the line that carries its figures, `$` apart.
    text(c, 112, y, "Income (loss) from")
    y -= 11
    text(c, 120, y, "continuing operations")
    leaders(c, 124 + stringWidth("continuing operations", FONT, 9), cols[0] - w - 6, y)
    for x, f in zip(cols, ["4,131", "1,311", "(1,002)"]):
        text(c, x - w, y, "$")
        right(c, x, y, f)
    p = c.beginPath()
    for x0, x1, yy in rules:
        p.moveTo(x0, yy)
        p.lineTo(x1, yy)
    c.setLineWidth(0.4)
    c.drawPath(p, stroke=1, fill=0)
    paragraph(c, 72, y - 30, ["Certain share options were not included in the computation of diluted earnings",
                              "per share because they would have been antidilutive."])
    c.showPage()
    c.save()
    heads = ["", "Fiscal / 2012 / (in millions)", "Fiscal / 2011 / (in millions)", "Fiscal / 2010 / (in millions)"]
    rows = [{"section": "Weighted-average shares outstanding:"}]
    rows += [{"cells": [label, *figs]} for label, figs, _ in body]
    rows.append({"cells": ["Income (loss) from continuing operations", "$ 4,131", "$ 1,311", "$(1,002)"]})
    return path, {"family": "statement", "formats": ["pdf"],
                  "blocks": [{"table": {"columns": heads, "all_cells": True, "rows": rows}}], "parts": []}


def booktabs():
    path = OUT / "probe-booktabs.pdf"
    c = canvas.Canvas(str(path), pagesize=(595, 842))
    text(c, 60, 770, "Table 2. Growth in remittance inflows", 9, BOLD)
    x0, x1 = 60, 535
    c.setLineWidth(0.8)
    c.line(x0, 762, x1, 762)
    years = ["2000-2004", "2004-2009", "2009-2014"]
    xs = [200, 280, 360]  # centres of the year columns
    centre(c, 280, 750, "Average annual growth", 8)
    c.setLineWidth(0.4)
    c.line(170, 746, 390, 746)
    for x, yr in zip(xs, years):
        centre(c, x, 736, yr, 8)
    text(c, 66, 743, "Country", 8)
    centre(c, 470, 750, "Inflows in 2020", 8)
    centre(c, 470, 740, "(US$ million)", 8)
    c.setLineWidth(0.8)
    c.line(x0, 730, x1, 730)
    data = [("Cambodia", ["7.5%", "-0.7%", "50.6%"], "1,272"),
            ("Indonesia", ["9.4%", "29.5%", "4.7%"], "9,651"),
            ("Lao PDR", ["4.0%", "115.7%", "38.0%"], "265"),
            ("Malaysia", ["18.6%", "7.1%", "6.9%"], "1,454")]
    y = 717
    for name, figs, total in data:
        text(c, 66, y, name, 8)
        for x, f in zip(xs, figs):
            centre(c, x, y, f, 8)
        centre(c, 470, y, total, 8)
        y -= 13
    c.line(x0, y + 5, x1, y + 5)
    paragraph(c, 60, y - 20, ["Remittances fell in every member state in 2020, most of all in Indonesia,",
                              "where inflows were a sixth lower than the year before."])
    c.showPage()
    c.save()
    heads = ["Country", *[f"Average annual growth / {y}" for y in years], "Inflows in 2020 (US$ million)"]
    rows = [{"cells": [n, *f, t]} for n, f, t in data]
    return path, {"family": "booktabs", "formats": ["pdf"],
                  "blocks": [{"table": {"columns": heads, "all_cells": True, "rows": rows}}], "parts": []}


def statement_far():
    path = OUT / "probe-statement-far.pdf"
    c = canvas.Canvas(str(path), pagesize=(612, 792))
    paragraph(c, 60, 720, ["Had compensation cost been determined by the fair value method, net earnings",
                           "and earnings per share would have been the pro forma amounts below:"])
    cols = [440, 490, 540]
    for x, yr in zip(cols, ["2002", "2001", "2000"]):
        right(c, x, 680, yr, 10, BOLD)
    rows = [("Net earnings, as reported", ["$345.6", "$267.0", "$221.0"]),
            ("Deduct: Compensation expense", ["(17.1)", "(11.8)", "(9.9)"]),
            ("Pro forma", ["$328.5", "$255.2", "$211.1"])]
    y = 664
    for label, figs in rows:
        text(c, 60, y, label, 10)
        for x, f in zip(cols, figs):
            right(c, x, y, f, 10)
        y -= 16
    text(c, 60, y, "Basic net earnings per share:", 10)
    y -= 16
    rows2 = [("As reported", ["$1.75", "$1.36", "$1.13"]), ("Pro forma", ["$1.66", "$1.30", "$1.08"])]
    for label, figs in rows2:
        text(c, 72, y, label, 10)
        for x, f in zip(cols, figs):
            right(c, x, y, f, 10)
        y -= 16
    c.showPage()
    c.save()
    heads = ["", "2002", "2001", "2000"]
    out = [{"cells": [label, *figs]} for label, figs in rows]
    out.append({"section": "Basic net earnings per share:"})
    out += [{"cells": [label, *figs]} for label, figs in rows2]
    return path, {"family": "statement", "formats": ["pdf"],
                  "blocks": [{"table": {"columns": heads, "all_cells": True, "rows": out}}], "parts": []}


def main():
    for make in (statement, booktabs, statement_far):
        path, expect = make()
        path.with_name(path.stem + ".expect.json").write_text(json.dumps(expect, indent=1, ensure_ascii=False) + "\n")
        print(path)


if __name__ == "__main__":
    main()
