"""Write field-list-colons.pdf: field lists whose labels end in a colon.

  python3 eval/tough/fixtures/field-list-colons.py

Invented text, one US Letter page, standard Helvetica (no fonts embedded):

- a report's header block, bold labels over plain values, three of whose
  values carry figures;
- a product's fields, labels and values in one plain face, with every
  other value set 0.02 pt above its label, as a spec sheet that places
  each cell on its own baseline does;
- what is no field list: a chart's NOTE and SOURCE, whose values are
  sentences, and paragraphs that open on a label and wrap under it.
"""
import os
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "field-list-colons.pdf")


def esc(t):
    return t.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def main():
    lines = []  # (x, y, size, bold, text); PDF points, origin bottom-left

    def put(x, y, size, bold, text):
        lines.append((x, y, size, bold, text))

    put(72, 740, 18, True, "Field Report")
    y = 706
    for label, value in [
        ("Report no.:", "FR-0042"),
        ("Issued:", "2026-01-05"),
        ("Prepared by:", "J. Example"),
        ("Status:", "Final"),
    ]:
        put(72, y, 10, True, label)
        put(180, y, 10, False, value)
        y -= 16
    put(72, y - 14, 10, False, "The inspection covered the north site and found the equipment in working order.")

    put(72, 600, 14, True, "Product data")
    y = 576
    for i, (label, value) in enumerate([
        ("Material:", "Polypropylene"),
        ("Capacity:", "500 ml"),
        ("Neck Diameter:", "38 mm"),
        ("Weight:", "24 g"),
        ("Pack Size:", "24 bottles per case"),
        ("Supplied with cap fitted:", "Yes"),
    ]):
        put(72, y, 10, False, label)
        put(240, y + (0.02 if i % 2 else 0.0), 10, False, value)
        y -= 23

    put(72, 420, 14, True, "Monthly output")
    put(72, 400, 10, False, "Output rose in each of the last three months, led by the east plant.")
    put(72, 380, 8, False, "NOTE:")
    put(120, 380, 8, False, "Output is measured in finished units. All series are seasonally adjusted.")
    put(72, 369, 8, False, "SOURCE:")
    put(120, 369, 8, False, "Plant records; the operations office's own calculations from them.")

    put(72, 330, 14, True, "Change history")
    y = 306
    for text in [
        ("Change Note:", "CN 4410 - Move the sampling step ahead of the transfer"),
        (None, "and renumber the steps after it."),
        ("Change Note:", "CN 4425 - Correct the units in the mixing table."),
        ("Change Note:", "CN 4431 - Add the drying time to the second wash and"),
        (None, "the operator's check after it, and renumber the steps."),
    ]:
        label, rest = text
        if label:
            put(72, y, 10, False, label)
            put(150, y, 10, False, rest)
        else:
            put(72, y, 10, False, rest)
        y -= 13

    ops = []
    for x, y, size, bold, text in lines:
        ops.append(f"BT /{'F2' if bold else 'F1'} {size} Tf {x} {y:.2f} Td ({esc(text)}) Tj ET")
    content = zlib.compress("\n".join(ops).encode("latin-1"))
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R"
        b" /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>",
        b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(content) + content + b"\nendstream",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
    ]
    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for o in offsets:
        out += b"%010d 00000 n \n" % o
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    with open(OUT, "wb") as f:
        f.write(out)
    print(OUT)


if __name__ == "__main__":
    main()
