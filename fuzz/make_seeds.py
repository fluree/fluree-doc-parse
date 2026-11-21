# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdf>=5"]
# ///
"""Write the seed corpora of the fuzz targets under fuzz/seeds/<target>/.

Small, valid files of every format a target reads, each with the features
its reader knows (tables, merged cells, lists, notes, comments, pictures,
attachments), so a fuzzer starts from inputs that reach deep into a reader
rather than from bytes its first check refuses.

Flat OpenDocument sources are written by hand and converted by LibreOffice
(`soffice`, or set SOFFICE) to the office formats; XPS, RTF, CSV, e-mail,
Outlook messages, and a Word and an Excel file of merged cells are written
here directly; pypdf adds notes and an embedded
file to a PDF. Run from the repository root:

    uv run fuzz/make_seeds.py [--extra DIR ...] [--local-extra DIR ...] [--local fuzz/corpus]

Generated files of at most `--max-size` bytes (64 KiB) are seeds, kept in
the repository; larger ones go to the local corpus under `--local`, if
given. `--extra DIR` adds the files directly in DIR (hostile inputs kept
elsewhere) to the target their extension names: as seeds those of at most
`--max-extra` bytes (8 KiB), to the local corpus the rest. `--local-extra
DIR` adds them to the local corpus only.
"""

from __future__ import annotations

import argparse
import email.message
import email.policy
import io
import os
import shutil
import struct
import subprocess
import tempfile
import zipfile
import zlib
from pathlib import Path

SEEDS = Path(__file__).resolve().parent / "seeds"
SOFFICE = os.environ.get("SOFFICE", "soffice")
# RTF control words start with one; kept out of the literals that hold them.
BACKSLASH = chr(92)

# The detect target's first byte picks the name (fuzz_targets/detect.rs).
DETECT_NAMES = [
    "f", "f.pdf", "f.csv", "f.tsv", "f.txt", "f.xls", "f.xlsx", "f.xlsb", "f.doc", "f.docx",
    "f.ppt", "f.pptx", "f.odt", "f.ods", "f.odp", "f.rtf", "f.xps", "f.oxps", "f.eml", "f.msg",
    "f.vtt", "f.srt", "f.html", "f.md", "f.png", "f.jpg", "f.tif", "f.pages", "f.key",
    "f.numbers", "f.mht", "f.PDF",
]

# The target each extension's files seed.
TARGET_OF = {
    ".csv": "csv", ".tsv": "csv", ".xls": "xls", ".xlsb": "xlsb", ".doc": "doc", ".ppt": "ppt",
    ".odt": "odf", ".ods": "odf", ".odp": "odf", ".rtf": "rtf", ".xps": "xps", ".oxps": "xps",
    ".docx": "docx", ".xlsx": "xlsx", ".pptx": "pptx", ".pdf": "pdf", ".eml": "email",
    ".msg": "email",
}


def png(width: int = 4, height: int = 3) -> bytes:
    """A small RGB PNG."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    raw = b"".join(
        b"\0" + b"".join(bytes((x * 60 % 256, y * 80 % 256, 128)) for x in range(width))
        for y in range(height)
    )
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


# --- OpenDocument sources, converted by LibreOffice -----------------------

FODT = """<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"
 xmlns:xlink="http://www.w3.org/1999/xlink"
 xmlns:dc="http://purl.org/dc/elements/1.1/"
 xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text">
 <office:meta><dc:title>Seed report</dc:title><dc:creator>Ada Park</dc:creator></office:meta>
 <office:master-styles><style:master-page style:name="Standard">
  <style:header><text:p>Running header</text:p></style:header>
  <style:footer><text:p>Page footer</text:p></style:footer>
 </style:master-page></office:master-styles>
 <office:body><office:text>
  <text:h text:outline-level="1">Quarterly report</text:h>
  <text:p>First paragraph with a note<text:note text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>A footnote.</text:p></text:note-body></text:note> and an endnote<text:note text:note-class="endnote"><text:note-citation>i</text:note-citation><text:note-body><text:p>An endnote.</text:p></text:note-body></text:note>.</text:p>
  <text:p><office:annotation><dc:creator>Kai Moreno</dc:creator><dc:date>2026-07-16T08:42:00</dc:date><text:p>Check this figure.</text:p></office:annotation>Commented text <text:a xlink:href="https://example.com/">with a link</text:a>, émoji ✓ and RTL שלום.</text:p>
  <text:h text:outline-level="2">Items</text:h>
  <text:list><text:list-item><text:p>One</text:p></text:list-item><text:list-item><text:p>Two</text:p>
   <text:list><text:list-item><text:p>Two point one</text:p></text:list-item></text:list></text:list-item></text:list>
  <table:table table:name="T1"><table:table-column table:number-columns-repeated="3"/>
   <table:table-row><table:table-cell><text:p>Region</text:p></table:table-cell><table:table-cell><text:p>Q1</text:p></table:table-cell><table:table-cell><text:p>Q2</text:p></table:table-cell></table:table-row>
   <table:table-row><table:table-cell table:number-columns-spanned="2"><text:p>North (merged)</text:p></table:table-cell><table:covered-table-cell/><table:table-cell><text:p>12</text:p></table:table-cell></table:table-row>
   <table:table-row><table:table-cell><text:p>South</text:p></table:table-cell><table:table-cell><text:p>3.5</text:p></table:table-cell><table:table-cell><text:p>-4</text:p></table:table-cell></table:table-row>
  </table:table>
  <text:p><draw:frame svg:width="2cm" svg:height="1.5cm" draw:name="pic"><draw:image><office:binary-data>{png}</office:binary-data></draw:image><svg:desc>A small picture</svg:desc></draw:frame></text:p>
  <text:p>Last paragraph.</text:p>
 </office:text></office:body>
</office:document>
"""

FODS = """<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
 xmlns:dc="http://purl.org/dc/elements/1.1/"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0"
 xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2"
 office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.spreadsheet">
 <office:meta><dc:title>Seed workbook</dc:title></office:meta>
 <office:automatic-styles>
  <number:date-style style:name="D1"><number:year number:style="long"/><number:text>-</number:text><number:month number:style="long"/><number:text>-</number:text><number:day number:style="long"/></number:date-style>
  <number:percentage-style style:name="P1"><number:number number:decimal-places="1" number:min-integer-digits="1"/><number:text>%</number:text></number:percentage-style>
  <style:style style:name="cd" style:family="table-cell" style:data-style-name="D1"/>
  <style:style style:name="cp" style:family="table-cell" style:data-style-name="P1"/>
 </office:automatic-styles>
 <office:body><office:spreadsheet>
  <table:table table:name="Sales">
   <table:table-row><table:table-cell office:value-type="string"><text:p>Region</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Date</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Amount</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Share</text:p></table:table-cell></table:table-row>
   <table:table-row><table:table-cell office:value-type="string"><text:p>North</text:p></table:table-cell><table:table-cell table:style-name="cd" office:value-type="date" office:date-value="2026-07-16"><text:p>2026-07-16</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="1234.5"><text:p>1234.5</text:p></table:table-cell><table:table-cell table:style-name="cp" office:value-type="percentage" office:value="0.125"><text:p>12.5%</text:p></table:table-cell></table:table-row>
   <table:table-row><table:table-cell table:number-columns-spanned="2" office:value-type="string"><text:p>South (merged)</text:p></table:table-cell><table:covered-table-cell/><table:table-cell table:formula="of:=[.C2]*2" office:value-type="float" office:value="2469"><text:p>2469</text:p></table:table-cell><table:table-cell office:value-type="boolean" office:boolean-value="true"><text:p>TRUE</text:p></table:table-cell></table:table-row>
   <table:table-row table:number-rows-repeated="2"><table:table-cell table:number-columns-repeated="3" office:value-type="string"><text:p>rep</text:p></table:table-cell></table:table-row>
  </table:table>
  <table:table table:name="Notes &amp; more">
   <table:table-row><table:table-cell office:value-type="string"><text:p>Second sheet ✓</text:p></table:table-cell></table:table-row>
  </table:table>
 </office:spreadsheet></office:body>
</office:document>
"""

FODP = """<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:dc="http://purl.org/dc/elements/1.1/"
 office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.presentation">
 <office:meta><dc:title>Seed deck</dc:title></office:meta>
 <office:master-styles><style:master-page style:name="Default"/></office:master-styles>
 <office:body><office:presentation>
  <draw:page draw:name="Intro" draw:master-page-name="Default">
   <draw:frame presentation:class="title" svg:x="1cm" svg:y="1cm" svg:width="20cm" svg:height="3cm"><draw:text-box><text:p>Seed deck title</text:p></draw:text-box></draw:frame>
   <draw:frame presentation:class="outline" svg:x="1cm" svg:y="5cm" svg:width="20cm" svg:height="8cm"><draw:text-box>
    <text:list><text:list-item><text:p>First bullet</text:p></text:list-item><text:list-item><text:p>Second bullet</text:p><text:list><text:list-item><text:p>Nested</text:p></text:list-item></text:list></text:list-item></text:list>
   </draw:text-box></draw:frame>
   <presentation:notes><draw:frame presentation:class="notes" svg:x="1cm" svg:y="14cm" svg:width="18cm" svg:height="10cm"><draw:text-box><text:p>Speaker notes.</text:p></draw:text-box></draw:frame></presentation:notes>
  </draw:page>
  <draw:page draw:name="Table" draw:master-page-name="Default">
   <draw:frame svg:x="1cm" svg:y="2cm" svg:width="15cm" svg:height="4cm"><table:table>
    <table:table-column table:number-columns-repeated="2"/>
    <table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row>
    <table:table-row><table:table-cell><text:p>Alpha</text:p></table:table-cell><table:table-cell><text:p>42</text:p></table:table-cell></table:table-row>
   </table:table></draw:frame>
   <draw:frame svg:x="17cm" svg:y="2cm" svg:width="3cm" svg:height="2cm"><draw:image><office:binary-data>{png}</office:binary-data></draw:image><svg:title>Logo</svg:title></draw:frame>
  </draw:page>
 </office:presentation></office:body>
</office:document>
"""


def soffice(src: Path, fmt: str, out: Path) -> Path | None:
    """Convert `src` to `fmt` with LibreOffice; the file, or None where it failed."""
    with tempfile.TemporaryDirectory(dir=out) as profile:
        r = subprocess.run(
            [
                SOFFICE,
                f"-env:UserInstallation=file://{profile}",
                "--headless",
                "--convert-to",
                fmt,
                "--outdir",
                str(out),
                str(src),
            ],
            capture_output=True,
            timeout=180,
        )
    produced = out / (src.stem + "." + fmt.split(":")[0])
    if r.returncode != 0 or not produced.exists():
        print(f"  soffice {src.name} -> {fmt} failed: {r.stderr.decode(errors='replace')[:200]}")
        return None
    return produced


def office(work: Path) -> dict[str, list[tuple[str, bytes]]]:
    """The office formats LibreOffice writes from the flat sources."""
    import base64

    picture = base64.b64encode(png(8, 6)).decode()
    sources = {
        "seed_text.fodt": (FODT, ["odt", "docx", "doc", "rtf", "pdf"]),
        "seed_sheet.fods": (FODS, ["ods", "xlsx", "xls", "pdf", "csv"]),
        "seed_deck.fodp": (FODP, ["odp", "pptx", "ppt", "pdf"]),
    }
    out: dict[str, list[tuple[str, bytes]]] = {}
    for name, (xml, formats) in sources.items():
        src = work / name
        src.write_text(xml.replace("{png}", picture), encoding="utf-8")
        for fmt in formats:
            f = soffice(src, fmt, work)
            if f is None:
                continue
            target = TARGET_OF["." + fmt]
            out.setdefault(target, []).append((f.name, f.read_bytes()))
    return out


# --- Formats written here -------------------------------------------------

def zipped(parts: list[tuple[str, bytes]], stored_first: bool = False) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for i, (name, data) in enumerate(parts):
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = (
                zipfile.ZIP_STORED if stored_first and i == 0 else zipfile.ZIP_DEFLATED
            )
            z.writestr(info, data)
    return buf.getvalue()


def xps() -> list[tuple[str, bytes]]:
    rels = (
        b'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        b'<Relationship Id="r1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/FixedDocSeq.fdseq"/>'
        b'<Relationship Id="r2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="/docProps/core.xml"/>'
        b"</Relationships>"
    )
    core = (
        b'<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" '
        b'xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Seed XPS</dc:title><dc:creator>Ada Park</dc:creator></cp:coreProperties>'
    )
    types = (
        b'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        b'<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        b'<Default Extension="png" ContentType="image/png"/>'
        b'<Default Extension="fpage" ContentType="application/vnd.ms-package.xps-fixedpage+xml"/>'
        b"</Types>"
    )
    seq = b'<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06"><DocumentReference Source="/Documents/1/Doc.fdoc"/></FixedDocumentSequence>'
    doc = (
        b'<FixedDocument xmlns="http://schemas.microsoft.com/xps/2005/06">'
        b'<PageContent Source="/Documents/1/Pages/1.fpage"/><PageContent Source="/Documents/1/Pages/2.fpage"/></FixedDocument>'
    )
    page1 = (
        '<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="816" Height="1056" xml:lang="en-US">'
        '<Canvas RenderTransform="1,0,0,1,48,48">'
        '<Glyphs OriginX="0" OriginY="20" FontRenderingEmSize="24" FontUri="/Resources/f.odttf" UnicodeString="Seed heading" Fill="#ff000000"/>'
        '<Glyphs OriginX="0" OriginY="60" FontRenderingEmSize="11" FontUri="/Resources/f.odttf" UnicodeString="A paragraph of body text on the first page." Indices=";;;;,50"/>'
        '<Glyphs OriginX="0" OriginY="76" FontRenderingEmSize="11" FontUri="/Resources/f.odttf" UnicodeString="{}{}Escaped braces and ✓ marks."/>'
        '<Path Data="M 0,100 L 400,100" Stroke="#ff000000" StrokeThickness="1"/>'
        '<Glyphs OriginX="0" OriginY="120" FontRenderingEmSize="11" UnicodeString="Name"/>'
        '<Glyphs OriginX="200" OriginY="120" FontRenderingEmSize="11" UnicodeString="Value"/>'
        '<Glyphs OriginX="0" OriginY="140" FontRenderingEmSize="11" UnicodeString="Alpha"/>'
        '<Glyphs OriginX="200" OriginY="140" FontRenderingEmSize="11" UnicodeString="42"/>'
        '<Glyphs OriginX="0" OriginY="160" FontRenderingEmSize="11" BidiLevel="1" UnicodeString="שלום"/>'
        "</Canvas></FixedPage>"
    ).encode()
    page2 = (
        b'<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="816" Height="1056">'
        b'<Path Data="M 48,48 L 448,48 L 448,348 L 48,348 Z"><Path.Fill><ImageBrush ImageSource="/Resources/img.png" '
        b'Viewbox="0,0,8,6" ViewboxUnits="Absolute" Viewport="48,48,400,300" ViewportUnits="Absolute"/></Path.Fill></Path>'
        b"</FixedPage>"
    )
    parts = [
        ("[Content_Types].xml", types),
        ("_rels/.rels", rels),
        ("docProps/core.xml", core),
        ("FixedDocSeq.fdseq", seq),
        ("Documents/1/Doc.fdoc", doc),
        ("Documents/1/Pages/1.fpage", page1),
        ("Documents/1/Pages/2.fpage", page2),
        ("Resources/img.png", png(8, 6)),
    ]
    return [("seed.xps", zipped(parts))]


def rtf() -> list[tuple[str, bytes]]:
    doc = (
        r"{\rtf1\ansi\ansicpg1252\deff0\uc1{\fonttbl{\f0\froman Times;}{\f1\fnil\fcharset128 MS Gothic;}}"
        r"{\colortbl;\red255\green0\blue0;}{\info{\title Seed RTF}{\author Ada Park}}"
        r"{\header \pard Running header\par}"
        r"{\pard\s1\outlinelevel0\b\fs32 Heading one\par}"
        r"\pard Body text with \i italics\i0 , a footnote{\super\chftn}{\footnote\pard\plain{\super\chftn} The note.}"
        r" and unicode " + BACKSLASH + r"u10003? and \'e9t\'e9.\par"
        r"{\f1\'82\'a0 Japanese}\par"
        r"{\*\annotation{\*\atnauthor Kai} A comment.}"
        r"{\field{\*\fldinst HYPERLINK \"https://example.com/\"}{\fldrslt link text}}\par"
        r"{\pntext\'b7\tab}{\*\pn\pnlvlblt}\pard\ls1\ilvl0 Bullet one\par"
        r"\trowd\cellx2000\cellx4000\intbl Name\cell Value\cell\row"
        r"\trowd\clmgf\cellx2000\clmrg\cellx4000\intbl Merged\cell\cell\row"
        r"\pard After the table.\par}"
    )
    return [("seed.rtf", doc.encode("latin-1"))]


def docx() -> list[tuple[str, bytes]]:
    """A Word table merged each way: a cell down two rows (`w:vMerge`), one
    across two columns (`w:gridSpan`), a band across the table."""
    w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"

    def tc(text: str, pr: str = "") -> str:
        return f"<w:tc><w:tcPr>{pr}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"

    rows = [
        tc("Group") + tc("Code") + tc("Note"),
        tc("Fruit", '<w:vMerge w:val="restart"/>') + tc("F-1") + tc("Fresh"),
        tc("", "<w:vMerge/>") + tc("F-2") + tc("Dried"),
        tc("Vegetables", '<w:gridSpan w:val="3"/>'),
        tc("Leaf") + tc("Spans two", '<w:gridSpan w:val="2"/>'),
    ]
    table = "<w:tbl>" + "".join(f"<w:tr>{r}</w:tr>" for r in rows) + "</w:tbl>"
    document = (
        f'<w:document xmlns:w="{w}"><w:body><w:p><w:r><w:t>Merged cells</w:t></w:r></w:p>'
        f"{table}</w:body></w:document>"
    )
    types = (
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
        "</Types>"
    )
    rels = (
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
        "</Relationships>"
    )
    parts = [("[Content_Types].xml", types), ("_rels/.rels", rels), ("word/document.xml", document)]
    return [("seed_merges.docx", zipped([(n, d.encode()) for n, d in parts]))]


def xlsx() -> list[tuple[str, bytes]]:
    """An Excel sheet whose table has a cell merged down over the cells it
    covers, one merged across, and a band across the table."""
    main = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"

    def cell(ref: str, value: str) -> str:
        if value.lstrip("-").replace(".", "", 1).isdigit():
            return f'<c r="{ref}"><v>{value}</v></c>'
        return f'<c r="{ref}" t="inlineStr"><is><t>{value}</t></is></c>'

    grid = [
        ["Group", "Q1", "Q2"],
        ["Fruit", "12", "14"],
        ["", "3", "4"],
        ["Vegetables", "", ""],
        ["Leaf", "Both quarters", ""],
        ["Root", "7", "8"],
    ]
    rows = "".join(
        f'<row r="{r}">'
        + "".join(cell(f"{'ABC'[c]}{r}", v) for c, v in enumerate(values) if v)
        + "</row>"
        for r, values in enumerate(grid, start=1)
    )
    merges = '<mergeCells count="3"><mergeCell ref="A2:A3"/><mergeCell ref="A4:C4"/><mergeCell ref="B5:C5"/></mergeCells>'
    sheet = f'<worksheet xmlns="{main}"><sheetData>{rows}</sheetData>{merges}</worksheet>'
    workbook = (
        f'<workbook xmlns="{main}" xmlns:r="{rel}"><sheets>'
        '<sheet name="Merges" sheetId="1" r:id="rId1"/></sheets></workbook>'
    )
    types = (
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        "</Types>"
    )
    package_rels = (
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>'
    )
    workbook_rels = (
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>'
    )
    parts = [
        ("[Content_Types].xml", types),
        ("_rels/.rels", package_rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", workbook_rels),
        ("xl/worksheets/sheet1.xml", sheet),
    ]
    return [("seed_merges.xlsx", zipped([(n, d.encode()) for n, d in parts]))]


def csv() -> list[tuple[str, bytes]]:
    return [
        ("seed.csv", b'Region,Amount,Note\r\nNorth,1234.5,"quoted, with comma"\r\nSouth,-4,"multi\nline"\r\n'),
        ("seed_semi.csv", "Région;Montant\nNord;1 234,5\nSud;3,5\n".encode("cp1252")),
        ("seed_utf16.csv", "﻿Name\tValue\nAlpha\t42\nBeta\t✓\n".encode("utf-16-le")),
        ("seed.tsv", b"a\tb\tc\n1\t2\t3\n\t\t\n4\t5\n"),
    ]


def eml() -> list[tuple[str, bytes]]:
    inner = email.message.EmailMessage(policy=email.policy.SMTP)
    inner["From"] = "Kai Moreno <kai@example.com>"
    inner["To"] = "ops@example.com"
    inner["Subject"] = "Budget"
    inner["Date"] = "Thu, 16 Jul 2026 08:42:00 +0000"
    inner.set_content("Can you approve it?\n")
    inner.add_attachment(b"col1,col2\n1,2\n", maintype="text", subtype="csv", filename="data.csv")

    msg = email.message.EmailMessage(policy=email.policy.SMTP)
    msg["From"] = "=?utf-8?q?L=C3=A9na_Holt?= <lena@example.com>"
    msg["To"] = "Kai Moreno <kai@example.com>, ops@example.com"
    msg["Cc"] = "audit@example.com"
    msg["Subject"] = "RE: Budget ✓"
    msg["Date"] = "Fri, 17 Jul 2026 09:00:00 +0200"
    msg["Message-ID"] = "<seed-1@example.com>"
    msg.set_content(
        "Approved.\n\nFrom: Kai Moreno <kai@example.com>\nSent: Thursday, July 16, 2026 8:42 AM\n"
        "Subject: Budget\n\n> Can you approve it?\n"
    )
    msg.add_alternative(
        '<html><body><p>Approved.</p><p><img src="cid:logo@x" alt="Logo"></p>'
        "<table><tr><th>Item</th><th>Cost</th></tr><tr><td>A</td><td>1</td></tr></table>"
        "<blockquote>Can you approve it?</blockquote></body></html>",
        subtype="html",
    )
    msg.get_payload()[1].add_related(png(), maintype="image", subtype="png", cid="<logo@x>")
    msg.add_attachment(b"%PDF-1.4\n%%EOF\n", maintype="application", subtype="pdf", filename="report.pdf")
    msg.add_attachment(inner)
    plain = (
        b"From: a@example.com\r\nTo: b@example.com\r\nSubject: =?iso-8859-1?b?Q2Fm6Q==?=\r\n"
        b"Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n"
        b"Caf=E9 au lait=\r\n continues.\r\n"
    )
    return [("seed.eml", msg.as_bytes()), ("seed_qp.eml", plain)]


# --- A minimal compound-file writer, for Outlook's .msg -------------------

def cfb(entries: list[tuple[str, bytes | None]]) -> bytes:
    """A version 3 compound file holding `entries`: ("a/b", bytes) for a
    stream, ("a", None) for a storage. Streams under 4 KiB live in the mini
    stream, as every Outlook message's do; larger ones are not supported."""
    SECT, MINI = 512, 64
    FREE, END, FAT_SECT = 0xFFFFFFFF, 0xFFFFFFFE, 0xFFFFFFFD
    STORAGE, STREAM, ROOT = 1, 2, 5

    # Directory entries: 0 is the root; children of each storage in a tree.
    nodes = [{"name": "Root Entry", "type": ROOT, "children": [], "data": b""}]
    index: dict[str, int] = {"": 0}
    for path, data in entries:
        parent = path.rpartition("/")[0]
        n = {"name": path.rpartition("/")[2], "type": STREAM if data is not None else STORAGE,
             "children": [], "data": data or b""}
        index[path] = len(nodes)
        nodes.append(n)
        nodes[index[parent]]["children"].append(index[path])

    # Mini stream.
    mini = bytearray()
    minifat: list[int] = []
    for n in nodes[1:]:
        if n["type"] != STREAM:
            continue
        assert len(n["data"]) < 4096
        if not n["data"]:
            n["start"] = END
            continue
        count = (len(n["data"]) + MINI - 1) // MINI
        n["start"] = len(minifat)
        minifat.extend(range(len(minifat) + 1, len(minifat) + count))
        minifat.append(END)
        mini += n["data"].ljust(count * MINI, b"\0")

    def key(i: int):
        name = nodes[i]["name"]
        return (len(name), name.upper())

    # Each storage's children as a balanced binary search tree, all black.
    left = [FREE] * len(nodes)
    right = [FREE] * len(nodes)
    child = [FREE] * len(nodes)

    def tree(ids: list[int]) -> int:
        if not ids:
            return FREE
        mid = len(ids) // 2
        left[ids[mid]] = tree(ids[:mid])
        right[ids[mid]] = tree(ids[mid + 1:])
        return ids[mid]

    for i, n in enumerate(nodes):
        child[i] = tree(sorted(n["children"], key=key))

    # Sector layout: [FAT][directory][mini stream][mini FAT].
    def sectors(n: int) -> int:
        return (n + SECT - 1) // SECT

    dir_bytes = len(nodes) * 128
    n_dir, n_mini, n_minifat = sectors(dir_bytes), sectors(len(mini)), sectors(len(minifat) * 4)
    n_fat = 1
    while (n_fat + n_dir + n_mini + n_minifat) > n_fat * (SECT // 4):
        n_fat += 1
    fat: list[int] = [FAT_SECT] * n_fat
    def chain(count: int) -> int:
        if count == 0:
            return END
        start = len(fat)
        fat.extend(range(start + 1, start + count))
        fat.append(END)
        return start
    dir_start, mini_start, minifat_start = chain(n_dir), chain(n_mini), chain(n_minifat)
    fat += [FREE] * (n_fat * (SECT // 4) - len(fat))

    nodes[0]["start"] = mini_start
    nodes[0]["data"] = bytes(mini)

    def entry(i: int) -> bytes:
        n = nodes[i]
        name = n["name"].encode("utf-16-le") + b"\0\0"
        return (
            name.ljust(64, b"\0")
            + struct.pack("<HBB", len(name), n["type"], 1)
            + struct.pack("<III", left[i], right[i], child[i])
            + b"\0" * 16 + b"\0" * 4 + b"\0" * 16
            + struct.pack("<IQ", n.get("start", END) if n["type"] != STORAGE else 0,
                          len(n["data"]) if n["type"] != STORAGE else 0)
        )

    directory = b"".join(entry(i) for i in range(len(nodes)))
    header = (
        bytes.fromhex("D0CF11E0A1B11AE1") + b"\0" * 16
        + struct.pack("<HHHHH", 0x3E, 3, 0xFFFE, 9, 6) + b"\0" * 6
        + struct.pack("<IIIIIIIII", 0, n_fat, dir_start, 0, 4096,
                      minifat_start if n_minifat else END, n_minifat, END, 0)
        + b"".join(struct.pack("<I", i) for i in range(n_fat))
        + struct.pack("<I", FREE) * (109 - n_fat)
    )
    body = b"".join(struct.pack("<I", x) for x in fat)
    body += directory.ljust(n_dir * SECT, b"\0")
    body += bytes(mini).ljust(n_mini * SECT, b"\0")
    body += b"".join(struct.pack("<I", x) for x in minifat).ljust(n_minifat * SECT, b"\xff")
    return header.ljust(SECT, b"\0") + body


def msg() -> list[tuple[str, bytes]]:
    def u(s: str) -> bytes:
        return s.encode("utf-16-le")

    def prop(tag: int, kind: int, value: int) -> bytes:
        return struct.pack("<IIQ", (tag << 16) | kind, 6, value)

    root_props = b"\0" * 32 + prop(0x0039, 0x0040, 134_287_876_800_000_000)
    entries: list[tuple[str, bytes | None]] = [
        ("__properties_version1.0", root_props),
        ("__substg1.0_0037001F", u("RE: Budget")),
        ("__substg1.0_0C1A001F", u("Lena Holt")),
        ("__substg1.0_5D01001F", u("lena@example.com")),
        ("__substg1.0_1000001F", u("Approved.\r\n\r\nFrom: Kai Moreno <kai@example.com>\r\n"
                                     "Sent: Thursday, July 16, 2026 8:42 AM\r\nSubject: Budget\r\n\r\n"
                                     "Can you approve it?\r\n")),
        ("__substg1.0_1013001F", u("<html><body><p>Approved.</p><img src=\"cid:img1\"></body></html>")),
    ]
    for i, (name, addr, kind) in enumerate([("Kai Moreno", "kai@example.com", 1),
                                             ("Ops Team", "ops@example.com", 2)]):
        d = f"__recip_version1.0_#{i:08X}"
        entries += [
            (d, None),
            (f"{d}/__properties_version1.0", b"\0" * 8 + prop(0x0C15, 0x0003, kind)),
            (f"{d}/__substg1.0_3001001F", u(name)),
            (f"{d}/__substg1.0_39FE001F", u(addr)),
        ]
    a0 = "__attach_version1.0_#00000000"
    entries += [
        (a0, None),
        (f"{a0}/__properties_version1.0", b"\0" * 8 + prop(0x3705, 0x0003, 1)),
        (f"{a0}/__substg1.0_3707001F", u("data.csv")),
        (f"{a0}/__substg1.0_370E001F", u("text/csv")),
        (f"{a0}/__substg1.0_37010102", b"a,b\n1,2\n"),
    ]
    a1 = "__attach_version1.0_#00000001"
    entries += [
        (a1, None),
        (f"{a1}/__properties_version1.0", b"\0" * 8 + prop(0x3705, 0x0003, 1)),
        (f"{a1}/__substg1.0_3707001F", u("img1.png")),
        (f"{a1}/__substg1.0_3712001F", u("img1")),
        (f"{a1}/__substg1.0_370E001F", u("image/png")),
        (f"{a1}/__substg1.0_37010102", png()),
    ]
    return [("seed.msg", cfb(entries))]


def annotated_pdf(base: bytes) -> bytes:
    """`base` with a note, a free-text note and a file embedded."""
    from pypdf import PdfReader, PdfWriter
    from pypdf.annotations import FreeText, Text

    w = PdfWriter(clone_from=PdfReader(io.BytesIO(base)))
    w.add_annotation(0, Text(rect=(50, 550, 200, 650), text="A sticky note", open=True))
    w.add_annotation(0, FreeText(text="Free text note", rect=(50, 400, 300, 450)))
    w.add_attachment("data.csv", b"a,b\n1,2\n")
    w.add_metadata({"/Title": "Seed PDF", "/Author": "Ada Park"})
    out = io.BytesIO()
    w.write(out)
    return out.getvalue()


def with_prefix(target: str, name: str, data: bytes) -> bytes:
    """`data` as `target` takes it: the CSV target's first byte picks the
    delimiter (odd: tab), the detect target's the name."""
    if target == "csv":
        return bytes([1 if name.endswith(".tsv") else 0]) + data
    return data


def detection(name: str, data: bytes, anonymous: bool) -> tuple[str, bytes]:
    """A detection seed: `data` under the name its extension is, or none."""
    if anonymous:
        return "anon_" + name, b"\0" + data
    ext = Path(name).suffix.lower()
    pick = next((i for i, n in enumerate(DETECT_NAMES) if n.endswith(ext)), 0)
    return name, bytes([pick]) + data


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--extra", action="append", default=[], type=Path,
                    help="hostile inputs: seeds where small, local corpus where not")
    ap.add_argument("--local-extra", action="append", default=[], type=Path,
                    help="inputs for the local corpus only")
    ap.add_argument("--local", type=Path, help="local corpus root (corpus/<target>/)")
    ap.add_argument("--max-size", type=int, default=64 << 10,
                    help="largest generated file kept as a seed")
    ap.add_argument("--max-extra", type=int, default=8 << 10,
                    help="largest --extra file kept as a seed")
    ap.add_argument("--out", type=Path, default=SEEDS)
    args = ap.parse_args()

    generated: dict[str, list[tuple[str, bytes]]] = {}
    # Under the fuzz directory, not /tmp: a confined LibreOffice (a snap)
    # sees neither the system's /tmp nor what is written there.
    with tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parent) as tmp:
        for target, files in office(Path(tmp)).items():
            generated.setdefault(target, []).extend(files)
    for target, files in [("xps", xps()), ("rtf", rtf()), ("csv", csv()), ("email", eml()),
                          ("email", msg()), ("docx", docx()), ("xlsx", xlsx())]:
        generated.setdefault(target, []).extend(files)
    # The text's PDF, with notes and an embedded file added.
    for name, data in list(generated.get("pdf", [])):
        if name == "seed_text.pdf":
            generated["pdf"].append(("seed_annotated.pdf", annotated_pdf(data)))

    seeds: dict[str, list[tuple[str, bytes]]] = {}
    local: dict[str, list[tuple[str, bytes]]] = {}

    def keep(target: str, name: str, data: bytes, small: bool) -> None:
        (seeds if small else local).setdefault(target, []).append(
            (name, with_prefix(target, name, data)))

    # Every generated file seeds its reader and detection, named and not.
    for target, files in generated.items():
        for name, data in files:
            small = len(data) <= args.max_size
            keep(target, name, data, small)
            for anonymous in (False, True):
                n, d = detection(name, data, anonymous)
                (seeds if small else local).setdefault("detect", []).append((n, d))
    for extra, committed in [(e, True) for e in args.extra] + [(e, False) for e in args.local_extra]:
        for f in sorted(extra.glob("*")):
            target = TARGET_OF.get(f.suffix.lower())
            if not (f.is_file() and target):
                continue
            data = f.read_bytes()
            small = committed and len(data) <= args.max_extra
            keep(target, f.name, data, small)
            n, d = detection(f.name, data, anonymous=False)
            local.setdefault("detect", []).append((n, d))

    if args.out.exists():
        shutil.rmtree(args.out)
    for root, sets in [(args.out, seeds), (args.local, local)]:
        if root is None:
            continue
        for target, files in sorted(sets.items()):
            d = root / target
            d.mkdir(parents=True, exist_ok=True)
            for name, data in files:
                (d / name).write_bytes(data)
            print(f"{root.name}/{target:6} {len(files):4} files, {sum(len(b) for _, b in files):8} bytes")


if __name__ == "__main__":
    main()
