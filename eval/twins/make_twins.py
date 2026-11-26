# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "pillow==11.0.0",
#   "pypdfium2==4.30.0",
#   "numpy==2.1.3",
# ]
# [tool.uv]
# python-preference = "only-managed"
# ///
"""Write the twins set: born-digital pages and their images, for OCR to match.

Each page is a born-digital PDF written here in the standard 14 fonts
(Helvetica, Times, Courier), with paragraphs set 1.3 em apart, short lines at
body size alone on their line (`Total`, `Bill to:`), and headings set larger
than the body and not bold, or at its size in bold: one word, or a line. fdoc reads the PDF's text
layer at its exact sizes; its twin is the same page rasterised by pdfium,
which only OCR reads. What the check holds OCR to is the twin: a heading the
born-digital page reads is a heading the image should read, and a line it
does not is not.

Sets, each written to corpus/<set>/ as <page>.pdf and <page>-<dpi>.png:

  plain-1.2, plain-1.3   headings a fifth and a third larger, at 150, 200 and 300 dpi
  bold                   headings at body size in the bold face, at 150, 200 and 300 dpi
  underline, boxed, strike, superscript, italic
                         headings a fifth larger with rules, note marks or
                         italic faces drawn on the page, at 200 dpi
  blue-ink, red-ink, grey-ink, inverted, white-on-blue, yellow-paper,
  gradient, photo-bg, noise, blur, fax, skew, jpeg
                         plain-1.2's 200 dpi images recoloured or degraded

Each set's meta.json lists its pages' headings and short lines. Names and
figures are invented; the run writes the same bytes on every machine with
the pinned dependencies.

  uv run eval/twins/make_twins.py [--out DIR]
"""
import argparse, io, json, os, random, zlib
from pathlib import Path

import numpy as np
import pypdfium2 as pdfium
from PIL import Image, ImageFilter

HERE = Path(__file__).resolve().parent
SHORT = ["Total", "Date", "Bill to:", "Debit", "Credit", "Amount", "Balance",
         "Notes", "Paid", "Ship to:", "Subtotal", "Signed", "Phone", "Reference"]
HEAD_SHORT = ["Summary", "Payments", "Exclusions", "Contacts", "Coverage",
              "Fees", "Claims", "Terms"]
HEAD_LONG = ["Changes to your policy", "How to contact our office",
             "What this statement covers", "Payments received this year"]
WORDS = ("the account statement shows every payment made during the period "
         "and any balance left unpaid after the due date is charged interest "
         "at the rate set out in your agreement please keep this notice with "
         "your records and write to the billing desk with any question").split()
UPRIGHT = {"helv": "Helvetica", "times": "Times-Roman", "cour": "Courier"}
BOLD = {"Helvetica": "Helvetica-Bold", "Times-Roman": "Times-Bold", "Courier": "Courier-Bold"}
ITALIC = {"helv": "Helvetica-Oblique", "times": "Times-Italic", "cour": "Courier-Oblique"}


def width(text, size, font):
    """A line's rough advance: Courier 0.6 em a character, the others about 0.5."""
    return len(text) * size * (0.6 if font.startswith("Courier") else 0.5)


def pdf(path, font, lines, mode, heads=()):
    """One Letter page of `lines` (x, y, size, text) in `font`, with what
    `mode` draws on them: an underline under each line, a box round each
    short one, a strike through each line of prose, a raised `1` after each,
    or the headings (`heads`) in the bold face."""
    ops = []
    for x, y, size, text in lines:
        t = text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
        f = "F2" if mode == "bold" and text in heads else "F1"
        ops.append(f"BT /{f} {size:.2f} Tf {x:.2f} {y:.2f} Td ({t}) Tj ET")
        w = width(text, size, font)
        if mode == "underline":
            ops.append(f"0.6 w {x:.2f} {y - 0.15 * size:.2f} m {x + w:.2f} {y - 0.15 * size:.2f} l S")
        elif mode == "boxed" and len(text.split()) <= 3:
            ops.append(f"0.5 w {x - 3:.2f} {y - 0.35 * size:.2f} {w + 6:.2f} {1.25 * size:.2f} re S")
        elif mode == "strike" and len(text.split()) >= 6:
            ops.append(f"0.5 w {x:.2f} {y + 0.3 * size:.2f} m {x + w:.2f} {y + 0.3 * size:.2f} l S")
        elif mode == "superscript":
            ops.append(f"BT /F1 {size * 0.6:.2f} Tf {x + w + 1:.2f} {y + 0.4 * size:.2f} Td (1) Tj ET")
    content = zlib.compress("\n".join(ops).encode("latin-1"))
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
        b"/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>",
        b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(content) + content + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /%s /Encoding /WinAnsiEncoding >>" % font.encode(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /%s /Encoding /WinAnsiEncoding >>" % BOLD.get(font, font).encode(),
    ]
    out, offsets = bytearray(b"%PDF-1.7\n"), []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    out += b"".join(b"%010d 00000 n \n" % o for o in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    path.write_bytes(bytes(out))


def pages(scale, mode, seed=11):
    """Nine pages per face (three sizes, three draws): their name, font, lines,
    headings and short lines."""
    r = random.Random(seed)
    fonts = ITALIC if mode == "italic" else UPRIGHT
    for fam, font in fonts.items():
        for pt in (10, 11, 12):
            for variant in range(3):
                y, x0 = 792 - 58, 72
                lines, heads, shorts = [], [], []
                k = 0
                while y > 100:
                    kind = k % 3
                    if kind == 0:
                        h = r.choice(HEAD_SHORT if k % 2 else HEAD_LONG)
                        y -= pt * 0.8
                        lines.append((x0, y, pt * scale, h))
                        heads.append(h)
                        y -= pt * scale * 1.3
                    elif kind == 1:
                        for _ in range(r.randint(2, 4)):
                            line = " ".join(r.choice(WORDS) for _ in range(r.randint(9, 12)))
                            lines.append((x0, y, pt, line.capitalize()))
                            y -= pt * 1.3
                    else:
                        s = r.choice(SHORT)
                        lines.append((x0, y, pt, s))
                        shorts.append(s)
                        y -= pt * 1.3
                        line = " ".join(r.choice(WORDS) for _ in range(r.randint(9, 12)))
                        lines.append((x0, y, pt, line.capitalize()))
                        y -= pt * 1.3
                    k += 1
                yield f"{fam}-{pt}-{variant}", font, lines, heads, shorts


def render(pdf_path, dpi):
    return pdfium.PdfDocument(str(pdf_path))[0].render(scale=dpi / 72, grayscale=True).to_pil()


def colourise(gray, ink, paper):
    t = (gray.astype(float) / 255.0)[..., None]
    return (np.array(ink) * (1 - t) + np.array(paper) * t).clip(0, 255).astype(np.uint8)


def variants(gray, rng):
    """plain-1.2's image recoloured or degraded, by name."""
    h, w = gray.shape
    g = gray.astype(float)
    yield "blue-ink", colourise(gray, (20, 40, 160), (255, 255, 255))
    yield "red-ink", colourise(gray, (180, 20, 20), (255, 255, 255))
    yield "inverted", colourise(gray, (245, 245, 245), (25, 30, 40))
    yield "white-on-blue", colourise(gray, (255, 255, 255), (30, 70, 140))
    yield "yellow-paper", colourise(gray, (30, 30, 30), (250, 240, 190))
    yield "grey-ink", colourise(gray, (130, 130, 130), (255, 255, 255))
    grad = np.linspace(255, 150, w)[None, :] * np.ones((h, 1))
    yield "gradient", np.minimum(g, grad).clip(0, 255).astype(np.uint8)
    small = rng.random((max(2, h // 60), max(2, w // 60)))
    soft = np.asarray(Image.fromarray((small * 255).astype(np.uint8)).resize((w, h), Image.BICUBIC), float) / 255
    yield "photo-bg", np.minimum(g, 140 + 110 * soft).clip(0, 255).astype(np.uint8)
    noisy = g + rng.normal(0, 25, g.shape)
    sp = rng.random(g.shape)
    noisy[sp < 0.01] = 0
    noisy[sp > 0.99] = 255
    yield "noise", noisy.clip(0, 255).astype(np.uint8)
    yield "blur", np.asarray(Image.fromarray(gray).filter(ImageFilter.GaussianBlur(1.2)))
    yield "fax", np.where(g < 150, 0, 255).astype(np.uint8)
    yield "skew", np.asarray(Image.fromarray(gray).rotate(1.5, resample=Image.BICUBIC, fillcolor=255))
    buf = io.BytesIO()
    Image.fromarray(gray).save(buf, "JPEG", quality=25)
    yield "jpeg", np.asarray(Image.open(buf))


def write_set(out, name, scale, mode, dpis):
    d = out / name
    d.mkdir(parents=True, exist_ok=True)
    meta = []
    for page, font, lines, heads, shorts in pages(scale, mode):
        path = d / f"{page}.pdf"
        pdf(path, font, lines, mode, set(heads))
        for dpi in dpis:
            render(path, dpi).save(d / f"{page}-{dpi}.png")
        meta.append({"page": page, "font": font, "dpis": list(dpis), "headings": heads, "short": shorts})
    (d / "meta.json").write_text(json.dumps({"set": name, "scale": scale, "pages": meta}, indent=1))
    return meta


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", type=Path, default=HERE / "corpus")
    out = ap.parse_args().out
    plain = write_set(out, "plain-1.2", 1.2, "plain", (150, 200, 300))
    write_set(out, "plain-1.3", 1.3, "plain", (150, 200, 300))
    write_set(out, "bold", 1.0, "bold", (150, 200, 300))
    for mode in ("underline", "boxed", "strike", "superscript", "italic"):
        write_set(out, mode, 1.2, mode, (200,))
    rng = np.random.default_rng(5)
    made = {}
    for m in plain:
        gray = np.asarray(Image.open(out / "plain-1.2" / f"{m['page']}-200.png").convert("L"))
        for name, arr in variants(gray, rng):
            d = out / name
            d.mkdir(parents=True, exist_ok=True)
            Image.fromarray(arr).save(d / f"{m['page']}-200.png")
            made.setdefault(name, []).append({**m, "dpis": [200]})
    for name, meta in made.items():
        # The born-digital twin is plain-1.2's PDF.
        (out / name / "meta.json").write_text(json.dumps(
            {"set": name, "scale": 1.2, "twin": "plain-1.2", "pages": meta}, indent=1))
    print(f"{3 + 5 + len(made)} sets in {out}")


if __name__ == "__main__":
    main()
