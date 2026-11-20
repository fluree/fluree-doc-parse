# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "reportlab==4.2.5",
#   "pillow==11.0.0",
#   "pypdfium2==4.30.0",
#   "numpy==2.1.3",
# ]
# [tool.uv]
# python-preference = "only-managed"
# ///
"""Write the scanned-PDF set: synthetic business pages, scanned, with their gold.

Every page starts as a born-digital PDF drawn here (reportlab, standard
fonts), so what it says is known exactly. It is then rasterised (pdfium),
degraded the way a scanner, a fax or a re-saved JPEG degrades a page, and
written back as an image-only PDF with no text layer. The gold is the list of
facts drawn on it: names, identifiers, amounts, dates and phrases, each said
exactly once on its page; and its headings, the lines set apart in bold
outside a table (a letterhead, a `Re:` line, a signature, an invoice's
`INVOICE` and `Bill to:`, a statement's closing balance), which are what
fdoc makes headings of on the born-digital page. Bold inside a table (its
header cells, an invoice's total), a statement's subtitle in plain type and
a screenshot's title are not headings.

Six base documents (a letter, a statement with a ruled table and an invoice
with an unruled one, two variants each, in Helvetica, Times or Courier) go
through every scan profile, so a profile's numbers compare with another's on
the same pages:

| profile    | what it is                                                      |
|------------|-----------------------------------------------------------------|
| clean-300  | 8-bit grey at 300 dpi, sensor noise only                        |
| clean-200  | the same at 200 dpi                                             |
| clean-150  | the same at 150 dpi                                             |
| skew-300   | 300 dpi, the sheet fed 0.8 to 2 degrees off square              |
| noisy-300  | 300 dpi, grey paper, soft focus, heavy noise and speckle        |
| fax-200    | 1-bit at 200 dpi, thresholded from a noisy scan                 |
| jpeg-150   | 150 dpi stored as a quality-35 JPEG (DCTDecode)                 |
| fax-std    | 1-bit at fax standard resolution, 200 by 100 dpi (non-square)   |
| clean-100  | 8-bit grey at 100 dpi, an archive scan made to save space       |

Two more letters, `pasted`, are born digital: a letter whose text layer is
real, with a screenshot pasted into it, written twice, the screenshot
captured at 96 and at 192 pixels per inch (four documents, the same draw at
both densities). Their facts are split into those in the text layer and
those only in the pixels.

Pinned dependency versions, a uv-managed interpreter and fixed seeds make
the output byte-identical from run to run on one machine (not across
platforms: the interpreter's zlib and the wheels differ), so the set is
regenerated rather than committed:

    uv run eval/scanned/make_scans.py                 # into eval/scanned/corpus/
    uv run eval/scanned/make_scans.py --out DIR --seed 7
"""

import argparse
import hashlib
import io
import json
import random
import re
import zlib
from pathlib import Path

import numpy as np
import pypdfium2 as pdfium
from PIL import Image, ImageFilter
from reportlab.lib.utils import ImageReader
from reportlab.pdfbase.pdfmetrics import stringWidth
from reportlab.pdfgen import canvas

HERE = Path(__file__).resolve().parent
PAGE = (612, 792)  # US Letter, in points
FONTS = {
    "Helvetica": ("Helvetica", "Helvetica-Bold"),
    "Times": ("Times-Roman", "Times-Bold"),
    "Courier": ("Courier", "Courier-Bold"),
}

# Invented names and places only.
FIRST = ["Alex", "Jordan", "Morgan", "Casey", "Riley", "Taylor", "Jamie", "Avery", "Quinn", "Robin"]
LAST = ["Sample", "Placeholder", "Example", "Testwell", "Mockford", "Fictus", "Demo", "Specimen"]
STREETS = ["Harbor Lane", "Orchard Road", "Mill Street", "Juniper Court", "Station Avenue", "Kestrel Way"]
CITIES = [("Riverton", "OH", "45001"), ("Lakeside", "WA", "98002"), ("Fairview", "TX", "75003"),
          ("Brookfield", "VT", "05004"), ("Millbrook", "NC", "27005")]
COMPANIES = ["Bramble & Finch Ltd", "Copperleaf Insurance Co.", "Example Data Inc.", "Northgate Utilities",
             "Silverpine Bank", "Harrow Office Supply"]
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September",
          "October", "November", "December"]
ITEMS = ["Archive boxes, pack of 10", "Toner cartridge, black", "Desk lamp, LED", "Copy paper, A4 ream",
         "Label printer tape", "Whiteboard markers", "Ergonomic footrest", "Cable organiser kit",
         "Laminating pouches", "Stapler, heavy duty"]
PAYEES = ["Card payment, Greenway Grocers", "Transfer to savings", "Direct debit, Northgate Utilities",
          "Salary, Example Data Inc.", "Cash withdrawal", "Standing order, rent", "Refund, Harrow Office Supply",
          "Card payment, Kestrel Cafe", "Interest paid", "Monthly account fee"]


def rng_for(seed, key):
    """A random source fixed by the seed and a key, independent of call order."""
    return random.Random(f"{seed}:{key}")


def np_rng_for(seed, key):
    digest = hashlib.sha256(f"{seed}:{key}".encode()).digest()
    return np.random.default_rng(int.from_bytes(digest[:8], "big"))


def money(cents):
    """An amount in cents as it is printed, `-1,165.29` for -116529: the sign
    is set apart, since a floor division would carry a negative amount's
    cents into its units."""
    sign, cents = ("-" if cents < 0 else ""), abs(cents)
    return f"{sign}{cents // 100:,}.{cents % 100:02d}"


def a_date(r):
    year, month, day = 2026, r.randrange(12), r.randint(1, 28)
    return f"{MONTHS[month]} {day}, {year}", f"{year}-{month + 1:02d}-{day:02d}"


def a_person(r):
    return f"{r.choice(FIRST)} {r.choice(LAST)}"


def an_address(r):
    city, state, zipcode = r.choice(CITIES)
    return f"{r.randint(2, 980)} {r.choice(STREETS)}", f"{city}, {state} {zipcode}"


def wrap(s, font, size, width):
    lines, line = [], ""
    for word in s.split():
        cand = f"{line} {word}".strip()
        if line and stringWidth(cand, font, size) > width:
            lines.append(line)
            line = word
        else:
            line = cand
    return lines + ([line] if line else [])


class Page:
    """A reportlab canvas that records each fact it draws."""

    def __init__(self, family, size=PAGE):
        self.buf = io.BytesIO()
        self.c = canvas.Canvas(self.buf, pagesize=size, invariant=1)
        self.regular, self.bold = FONTS[family]
        self.facts = []
        self.headings = []  # the lines drawn as headings, in drawing order
        self.pixel_text = ""  # what a pasted screenshot says, which no text layer holds
        self._pdf = None

    def fact(self, text, kind, where="page"):
        self.facts.append({"text": text, "kind": kind, "where": where})

    def text(self, x, y, s, size=10, bold=False, align="left", heading=False):
        """Draw one line; `heading` records it as one of the page's headings."""
        if heading:
            self.headings.append(s)
        self.c.setFont(self.bold if bold else self.regular, size)
        {"left": self.c.drawString, "right": self.c.drawRightString,
         "centre": self.c.drawCentredString}[align](x, y, s)

    def paragraph(self, x, y, s, size=10, width=468, leading=1.35):
        lines = wrap(s, self.regular, size, width)
        for i, line in enumerate(lines):
            self.text(x, y - i * size * leading, line, size)
        return y - len(lines) * size * leading

    def pdf(self):
        if self._pdf is None:
            self.c.showPage()
            self.c.save()
            self._pdf = self.buf.getvalue()
        return self._pdf

    def said(self):
        """Everything the page says, its text layer and its screenshot's."""
        doc = pdfium.PdfDocument(self.pdf())
        try:
            text = doc[0].get_textpage().get_text_bounded()
        finally:
            doc.close()
        return norm(f"{text} {self.pixel_text}")


def norm(s):
    return re.sub(r"\s+", " ", s).strip()


def repeated_facts(page):
    """The facts a page does not say exactly once, strictly and as a whole
    word (the rules `check.py` finds them by). A fact said twice would be
    credited by whichever occurrence was read, not by the one the gold means."""
    said = page.said()
    out = []
    for f in page.facts:
        whole = re.findall(r"(?<![\w.,])" + re.escape(f["text"]) + r"(?![\w]|[.,]\d)", said)
        if said.count(f["text"]) != 1 or len(whole) != 1:
            out.append(f["text"])
    return out


def drawn(make, seed, key):
    """The page `make(r)` draws, drawn again from the next random source
    while any of its facts is not said exactly once on it. Every fact is
    then one place on the page, and only reading that place finds it."""
    for attempt in range(100):
        page = make(rng_for(seed, key if attempt == 0 else f"{key}/{attempt}"))
        repeated = repeated_facts(page)
        if not repeated:
            return page
    raise SystemExit(f"{key}: no draw says each fact once; still repeated: {repeated}")


def letter(r, family, page=None):
    """A business letter: letterhead, addresses, a reference and two paragraphs.
    The addressee and the company are named once, so each is one fact."""
    p = page or Page(family)
    company, person, signer = r.choice(COMPANIES), a_person(r), a_person(r)
    while signer == person:
        signer = a_person(r)
    street, city = an_address(r)
    long_date, _ = a_date(r)
    ref = f"PX-{r.randint(1000, 9999)}-{r.randint(10000, 99999)}"
    phone = f"+1 555 {r.randint(100, 999)} {r.randint(1000, 9999)}"
    amount = money(r.randint(20_000, 900_000))
    due_long, _ = a_date(r)

    p.text(72, 730, company, 15, bold=True, heading=True)
    p.text(72, 714, f"Customer service {phone}", 9)
    p.fact(company, "name")
    p.fact(phone, "id")
    p.text(540, 680, long_date, 10, align="right")
    p.fact(long_date, "date")
    for i, line in enumerate([person, street, city]):
        p.text(72, 650 - i * 13, line, 10)
    p.fact(person, "name")
    p.fact(street, "phrase")
    p.fact(city, "phrase")
    p.text(72, 596, f"Re: Policy reference {ref}", 10, bold=True, heading=True)
    p.fact(ref, "id")
    p.text(72, 572, "Dear Policyholder,", 10)
    first = (f"Thank you for your letter about the renewal of your policy. We have reviewed the account "
             f"and confirm that the outstanding balance of ${amount} is payable by {due_long}. "
             f"Please quote the reference above in any correspondence with our office.")
    y = p.paragraph(72, 550, first)
    p.fact(amount, "amount")
    p.fact(due_long, "date")
    p.fact("Please quote the reference above in any correspondence", "phrase")
    second = ("If you have already arranged payment, no further action is needed. Our team is available "
              "on weekdays between nine and five, and can also set up a payment plan on request.")
    y = p.paragraph(72, y - 12, second)
    p.fact("can also set up a payment plan on request", "phrase")
    p.text(72, y - 24, "Yours sincerely,", 10)
    p.text(72, y - 64, signer, 10, bold=True, heading=True)
    p.text(72, y - 77, "Accounts team", 10)
    p.fact(signer, "name")
    return p


def statement(r, family):
    """An account statement whose transactions sit in a fully ruled table."""
    p = Page(family)
    bank, holder = r.choice(["Silverpine Bank", "Riverton Savings & Loan"]), a_person(r)
    account = f"{r.randint(10, 99)}-{r.randint(1000, 9999)}-{r.randint(1000000, 9999999)}"
    month = r.randrange(11)
    period = f"{MONTHS[month]} 1 to {MONTHS[month]} 28, 2026"
    opening = r.randint(100_000, 900_000)

    p.text(72, 730, bank, 16, bold=True, heading=True)
    p.text(72, 712, "Statement of account", 11)
    p.fact(bank, "name")
    p.text(72, 684, f"Account holder: {holder}", 10)
    p.text(72, 670, f"Account number: {account}", 10)
    p.text(72, 656, f"Period: {period}", 10)
    p.fact(holder, "name")
    p.fact(account, "id")
    p.fact(period, "date")

    cols = [72, 130, 340, 410, 480, 540]  # column edges
    heads = ["Date", "Description", "Debit", "Credit", "Balance"]
    row_h, top = 18, 626
    gold_rows = (1, 4, 6)
    rows, balance = [], opening
    # Eight distinct labels: a gold row's label said again on another row
    # would be found there.
    for i, payee in enumerate(r.sample(PAYEES, 8)):
        day = 1 + i * 3 + r.randint(0, 2)
        cents = r.randint(500, 150_000)
        credit = payee.startswith(("Salary", "Refund", "Interest"))
        balance += cents if credit else -cents
        rows.append([f"{month + 1:02d}/{day:02d}", payee, "" if credit else money(cents),
                     money(cents) if credit else "", money(balance)])
    c = p.c
    c.setLineWidth(0.6)
    n = len(rows) + 1
    for i in range(n + 1):
        c.line(cols[0], top - i * row_h, cols[-1], top - i * row_h)
    for x in cols:
        c.line(x, top, x, top - n * row_h)
    for j, h in enumerate(heads):
        p.text(cols[j] + 4, top - 13, h, 9, bold=True)
    for i, row in enumerate(rows, start=1):
        y = top - i * row_h - 13
        p.text(cols[0] + 4, y, row[0], 9)
        p.text(cols[1] + 4, y, row[1], 9)
        for j in (2, 3, 4):
            if row[j]:
                p.text(cols[j + 1] - 4, y, row[j], 9, align="right")
    # Gold: the description, amount and balance of three rows, whole.
    for row in (rows[g] for g in gold_rows):
        p.fact(row[1], "phrase")
        p.fact(row[2] or row[3], "amount")
        p.fact(row[4], "amount")
    p.fact(rows[0][0], "date")
    y = top - n * row_h - 28
    p.text(72, y, f"Opening balance: {money(opening)}", 10)
    p.text(72, y - 14, f"Closing balance: {money(balance)}", 10, bold=True, heading=True)
    p.fact(money(opening), "amount")
    # The closing balance is also the last row's: the line is the fact.
    p.fact(f"Closing balance: {money(balance)}", "amount")
    p.paragraph(72, y - 44, "Report any transaction you do not recognise within sixty days of the statement "
                "date. Interest is calculated on the daily closing balance.", 9)
    p.fact("Interest is calculated on the daily closing balance", "phrase")
    return p


def invoice(r, family):
    """An invoice whose items sit in an unruled table, totals beneath it."""
    p = Page(family)
    seller, buyer = "Harrow Office Supply", a_person(r)
    number = f"INV-{r.randint(2026000, 2026999)}"
    _, iso_date = a_date(r)
    street, city = an_address(r)

    p.text(72, 730, seller, 16, bold=True, heading=True)
    p.text(540, 730, "INVOICE", 16, bold=True, align="right", heading=True)
    p.text(540, 712, f"Invoice no. {number}", 10, align="right")
    p.text(540, 698, f"Date: {iso_date}", 10, align="right")
    p.fact(number, "id")
    p.fact(iso_date, "date")
    p.text(72, 680, "Bill to:", 10, bold=True, heading=True)
    for i, line in enumerate([buyer, street, city]):
        p.text(72, 666 - i * 13, line, 10)
    p.fact(buyer, "name")
    p.fact(street, "phrase")

    heads = [("Item", 72, "left"), ("Qty", 360, "right"), ("Unit price", 450, "right"), ("Amount", 540, "right")]
    for h, x, align in heads:
        p.text(x, 610, h, 10, bold=True, align=align)
    items = r.sample(ITEMS, 6)
    subtotal = 0
    y = 590
    for k, item in enumerate(items):
        # At least two of each, so an amount is not also its unit price.
        qty, unit = r.randint(2, 12), r.randint(250, 12_000)
        subtotal += qty * unit
        p.text(72, y, item, 10)
        p.text(360, y, str(qty), 10, align="right")
        p.text(450, y, money(unit), 10, align="right")
        p.text(540, y, money(qty * unit), 10, align="right")
        if k in (0, 3, 5):
            p.fact(item, "phrase")
            p.fact(money(qty * unit), "amount")
        y -= 18
    tax = subtotal * 8 // 100
    for label, cents in [("Subtotal", subtotal), ("Sales tax 8%", tax), ("Total due", subtotal + tax)]:
        y -= 4
        p.text(450, y, label, 10, bold=label == "Total due", align="right")
        p.text(540, y, money(cents), 10, bold=label == "Total due", align="right")
        p.fact(money(cents), "amount")
        y -= 16
    p.paragraph(72, y - 30, "Payment is due within thirty days. Please write the invoice number on the back "
                "of any cheque, or use it as the reference of a bank transfer.", 9)
    p.fact("Payment is due within thirty days.", "phrase")
    return p


def screenshot(r, family, taken=()):
    """A small application window, as a vector PDF, with its facts. Its
    customer is none of the names in `taken`, so the page it is pasted into
    does not already say it."""
    w, h = 300, 150  # points; rasterised at the capture's own density
    p = Page(family, size=(w, h))
    c = p.c
    c.setFillGray(0.85)
    c.rect(0, h - 22, w, 22, stroke=0, fill=1)
    c.setFillGray(0)
    c.setStrokeGray(0.6)
    c.rect(0.5, 0.5, w - 1, h - 1, stroke=1, fill=0)
    order = f"ORD-{r.randint(100000, 999999)}"
    customer = a_person(r)
    while customer in taken:
        customer = a_person(r)
    total = money(r.randint(5_000, 400_000))
    status = r.choice(["Awaiting shipment", "Delivered to depot", "Payment received"])
    p.text(8, h - 15, "Order details", 10, bold=True)
    rows = [("Order", order, "id"), ("Customer", customer, "name"), ("Total", total, "amount"),
            ("Status", status, "phrase")]
    for i, (label, value, kind) in enumerate(rows):
        y = h - 44 - i * 24
        p.text(10, y, label, 10)
        p.text(100, y, value, 10, bold=True)
        p.fact(value, kind, where="pixels")
    return p


def pasted(r, family, ppi):
    """A born-digital letter with a screenshot pasted under its signature."""
    p = letter(r, family)
    shot = screenshot(r, family, taken={f["text"] for f in p.facts if f["kind"] == "name"})
    img = rasterise(shot.pdf(), ppi).convert("RGB")
    # Placed at its own pixel density, below the letter's body.
    w, h = img.width * 72 / ppi, img.height * 72 / ppi
    p.c.drawImage(ImageReader(img), 72, 90, width=w, height=h)
    # Its box as fdoc gives one, in points from the top left.
    p.screenshot = [72, PAGE[1] - 90 - h, 72 + w, PAGE[1] - 90]
    p.text(72, 80, "Screenshot of the order as it appears in the customer portal.", 8)
    p.fact("Screenshot of the order as it appears in the customer portal.", "phrase")
    p.facts += shot.facts
    p.pixel_text = shot.said()
    return p


def rasterise(pdf_bytes, dpi):
    doc = pdfium.PdfDocument(pdf_bytes)
    try:
        bitmap = doc[0].render(scale=dpi / 72, grayscale=True)
        return bitmap.to_pil().convert("L")
    finally:
        doc.close()


def add_noise(img, rng, sigma, white_point=None):
    """Gaussian sensor noise; a scanner's white point, where given, clips what
    is near paper white back to white."""
    a = np.asarray(img, dtype=np.float32)
    a = a + rng.normal(0, sigma, a.shape)
    if white_point is not None:
        a[a >= white_point] = 255
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8), "L")


def speckle(img, rng, fraction):
    a = np.array(img)
    mask = rng.random(a.shape) < fraction
    a[mask] = np.where(rng.random(mask.sum()) < 0.5, 0, 255)
    return Image.fromarray(a, "L")


# Each profile: (dpi, how the raster is degraded and stored); a pair is the
# horizontal and vertical resolution of the stored image. A degrade
# function takes the rendered page and a random source and returns
# (image, storage) where storage is "flate8", "flate1" or "jpeg".
def clean(img, rng):
    return add_noise(img, rng, 2.5, white_point=240), "flate8"


def skewed(img, rng):
    angle = rng.uniform(0.8, 2.0) * (1 if rng.random() < 0.5 else -1)
    img = img.rotate(angle, resample=Image.Resampling.BICUBIC, fillcolor=255)
    return add_noise(img, rng, 3, white_point=240), "flate8"


def noisy(img, rng):
    a = np.asarray(img, dtype=np.float32)
    img = Image.fromarray((228 - (255 - a) * 0.85).clip(0, 255).astype(np.uint8), "L")
    img = img.filter(ImageFilter.GaussianBlur(0.8))
    return speckle(add_noise(img, rng, 14), rng, 0.002), "flate8"


def fax(img, rng):
    img = add_noise(img.filter(ImageFilter.GaussianBlur(0.5)), rng, 22)
    img = speckle(img, rng, 0.0005)
    return img.point(lambda v: 255 if v > 150 else 0).convert("1", dither=Image.Dither.NONE), "flate1"


def jpeg(img, rng):
    return add_noise(img, rng, 3, white_point=240), "jpeg"


def fax_standard(img, rng):
    """Fax at standard resolution: every other scan line, so half the
    vertical resolution, then the same thresholding as `fax`."""
    img = img.resize((img.width, img.height // 2), Image.Resampling.BOX)
    return fax(img, rng)


PROFILES = {
    "clean-300": (300, clean),
    "clean-200": (200, clean),
    "clean-150": (150, clean),
    "skew-300": (300, skewed),
    "noisy-300": (300, noisy),
    "fax-200": (200, fax),
    "jpeg-150": (150, jpeg),
    "fax-std": ((200, 100), fax_standard),
    "clean-100": (100, clean),
}


def image_pdf(img, dpi, storage):
    """A one-page PDF holding nothing but this image, page-sized. `dpi` is one
    resolution, or the horizontal and vertical ones."""
    dpi_x, dpi_y = dpi if isinstance(dpi, tuple) else (dpi, dpi)
    if storage == "jpeg":
        out = io.BytesIO()
        img.save(out, "JPEG", quality=35, optimize=False)
        data, filt, bits = out.getvalue(), "/DCTDecode", 8
    elif storage == "flate1":
        data, filt, bits = zlib.compress(img.tobytes(), 6), "/FlateDecode", 1
    else:
        data, filt, bits = zlib.compress(img.tobytes(), 6), "/FlateDecode", 8
    w_pt, h_pt = img.width * 72 / dpi_x, img.height * 72 / dpi_y
    content = f"q {w_pt:.4f} 0 0 {h_pt:.4f} 0 0 cm /Im0 Do Q".encode()
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w_pt:.4f} {h_pt:.4f}] "
        f"/Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>".encode(),
        f"<< /Type /XObject /Subtype /Image /Width {img.width} /Height {img.height} "
        f"/ColorSpace /DeviceGray /BitsPerComponent {bits} /Filter {filt} /Length {len(data)} >>\n"
        f"stream\n".encode() + data + b"\nendstream",
        f"<< /Length {len(content)} >>\nstream\n".encode() + content + b"\nendstream",
    ]
    out = io.BytesIO()
    out.write(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objects, start=1):
        offsets.append(out.tell())
        out.write(f"{i} 0 obj\n".encode() + body + b"\nendobj\n")
    xref = out.tell()
    out.write(f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode())
    for off in offsets:
        out.write(f"{off:010d} 00000 n \n".encode())
    out.write(f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode())
    return out.getvalue()


def base_documents(seed):
    """The six documents every scan profile is applied to."""
    out = []
    for kind, make, families in [("letter", letter, ["Helvetica", "Times"]),
                                 ("statement", statement, ["Helvetica", "Courier"]),
                                 ("invoice", invoice, ["Times", "Helvetica"])]:
        for variant, family in enumerate(families, start=1):
            key = f"{kind}-{variant}"
            out.append((key, drawn(lambda r, make=make, family=family: make(r, family), seed, key)))
    return out


def write(out_dir, doc_id, pdf, gold):
    (out_dir / f"{doc_id}.pdf").write_bytes(pdf)
    (out_dir / f"{doc_id}.gold.json").write_text(json.dumps(gold, indent=1, ensure_ascii=False) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--out", type=Path, default=HERE / "corpus")
    ap.add_argument("--seed", type=int, default=4)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    index = []
    for base_id, page in base_documents(args.seed):
        vector = page.pdf()
        for profile, (dpi, degrade) in PROFILES.items():
            doc_id = f"{base_id}.{profile}"
            render_dpi = dpi[0] if isinstance(dpi, tuple) else dpi
            img, storage = degrade(rasterise(vector, render_dpi), np_rng_for(args.seed, doc_id))
            gold = {"id": doc_id, "base": base_id, "profile": profile, "dpi": dpi,
                    "text_layer": False, "facts": page.facts,
                    "headings": page.headings}
            write(args.out, doc_id, image_pdf(img, dpi, storage), gold)
            index.append(doc_id)
    for variant, family in [(1, "Helvetica"), (2, "Times")]:
        for ppi in (96, 192):
            doc_id = f"pasted-{variant}.screenshot-{ppi}"
            page = drawn(lambda r: pasted(r, family, ppi), args.seed, f"pasted-{variant}")
            gold = {"id": doc_id, "base": f"pasted-{variant}", "profile": f"pasted-{ppi}", "dpi": ppi,
                    "text_layer": True, "screenshot": page.screenshot, "facts": page.facts,
                    "headings": page.headings}
            write(args.out, doc_id, page.pdf(), gold)
            index.append(doc_id)
    (args.out / "index.json").write_text(json.dumps({"seed": args.seed, "documents": index}, indent=1) + "\n")
    print(f"{len(index)} documents in {args.out}")


if __name__ == "__main__":
    main()
