#!/usr/bin/env python3
"""Tough cases: real documents the engine once read wrong, checked so it does
not again.

Each case file names one document and the facts about it that a fix made
true: a row of a table read whole, under the right column names, or a column
name that must not appear. Every document is also held to the offset
contract: each element's string is exactly what its offsets cut from the
text projection.

    eval/tough/check.py                  # every case
    eval/tough/check.py --fetch          # download the public documents first
    eval/tough/check.py nist-sec         # one case, every failure shown
    eval/tough/check.py --fdoc PATH      # another binary

Public cases live in eval/tough/cases/ and fetch their document from its
publisher, checking it against the SHA-256 the checks were written on.
Cases whose document cannot be published live in eval/tough/private/, which
is not tracked, and name a local path instead. A check marked "open" records
a known failure: it does not fail the run, and the run says when it starts
passing. See eval/tough/README.md.
"""

import argparse
import hashlib
import json
import subprocess
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
DOCS = HERE / "docs"


def norm(s):
    return " ".join(str(s).split())


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_cases(dirs, only):
    cases = []
    for d in dirs:
        for f in sorted(Path(d).glob("*.json")):
            case = json.loads(f.read_text(encoding="utf-8"))
            case["_id"] = f.stem
            case["_dir"] = f.parent
            if not only or f.stem in only:
                cases.append(case)
    return cases


def document_path(case):
    doc = case["document"]
    if "path" in doc:
        p = Path(doc["path"]).expanduser()
        return p if p.is_absolute() else case["_dir"] / p
    return DOCS / doc["file"]


def fetch(case, path):
    url = case["document"]["url"]
    print(f"fetching {path.name} from {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (fdoc eval)"})
    path.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(req, timeout=60) as r:
        path.write_bytes(r.read())


def convert(fdoc, path, fmt, extra=()):
    out = subprocess.run(
        [fdoc, "convert", str(path), "--no-escalate", "-f", fmt, *extra],
        capture_output=True,
        check=True,
    ).stdout
    return out.decode("utf-8")


def convert_args(case):
    """Arguments a case's document is converted with: the OCR word readings
    it names, a directory relative to the case file, read as `--ocr-words`
    reads them. A reading stored with the case reads the same on every
    machine, with no engine or models."""
    words = case["document"].get("ocr_words")
    return ("--ocr-words", str(case["_dir"] / words)) if words else ()


class Reading:
    """A document's DoCO graph and text projection, indexed for the checks."""

    def __init__(self, doco, text, again=None):
        graph = json.loads(doco)["@graph"]
        self.doco = doco
        self.again = again
        self.text = text
        self.nodes = {n["@id"]: n for n in graph}
        self.cells = [n for n in graph if n.get("@type") == "doc:TableCell"]
        # A table's rows are keyed by the first part of the table they belong
        # to, so a table the page break split reads as one. A cell under no
        # column name is keyed by its column, `#0`.
        self.rows = defaultdict(dict)
        for t in (n for n in graph if n.get("@type") == "doco:Table"):
            first = t
            while first.get("doc:continues") in self.nodes:
                first = self.nodes[first["doc:continues"]]
            for cid in t.get("po:contains", []):
                c = self.nodes.get(cid, {})
                if c.get("@type") != "doc:TableCell":
                    continue
                key = (first["@id"], c["doc:rowIndex"])
                name = c.get("doc:columnHeader", f"#{c['doc:columnIndex']}")
                self.rows[key][norm(name)] = norm(c["doc:cellValue"])
        # Each section as (its title, its level, the title of the section
        # directly holding it), titles exactly as their `nif:isString`.
        title = lambda s: self.nodes.get(s.get("po:containsAsHeader"), {}).get("nif:isString")
        sections = [n for n in graph if n.get("@type") == "doco:Section"]
        held_by = {c: s for s in sections for c in s.get("po:contains", [])}
        self.sections = [
            (title(s), s.get("doc:sectionLevel"), title(held_by[s["@id"]]) if s["@id"] in held_by else None)
            for s in sections
        ]
        # The title of the section each element falls in, through any list
        # or figure that holds it.
        parent = {c: n["@id"] for n in graph for c in n.get("po:contains", [])}

        def section_of(eid):
            seen = set()
            while eid in parent and eid not in seen:
                seen.add(eid)
                eid = parent[eid]
                if self.nodes[eid].get("@type") == "doco:Section":
                    return title(self.nodes[eid])
            return None

        self.section_of = section_of
        # Each list's items, their strings in order.
        self.lists = [
            [self.nodes.get(c, {}).get("nif:isString") for c in n.get("po:contains", [])]
            for n in graph
            if n.get("@type") == "doco:List"
        ]
        # Each list nested in an item, under that item's string: the items
        # of every list the item contains.
        self.nested = defaultdict(list)
        for n in graph:
            for c in n.get("po:contains", []):
                sub = self.nodes.get(c, {})
                if n.get("doc:xhtmlTag") == "li" and sub.get("@type") == "doco:List":
                    self.nested[n.get("nif:isString", "")].append(
                        [self.nodes.get(k, {}).get("nif:isString") for k in sub.get("po:contains", [])]
                    )
        # Each page's printed number, by its place in the file from 1.
        pages = next((n["doc:pages"]["@value"] for n in graph if "doc:pages" in n), [])
        self.folios = {p["pageIndex"] + 1: p.get("folio") for p in pages}
        # The lines read as running text: heads and feet the pages repeat,
        # kept out of the body and listed once on the document.
        running = next((n["doc:runningText"] for n in graph if "doc:runningText" in n), [])
        if isinstance(running, dict):
            running = running.get("@value", [])
        self.running = [norm(t) for t in running]

    def offsets(self):
        """Elements whose string is not what their offsets cut."""
        bad = []
        for n in self.nodes.values():
            if "nif:beginIndex" not in n:
                continue
            b, e = int(n["nif:beginIndex"]), int(n["nif:endIndex"])
            want = n.get("nif:isString", n.get("doc:cellValue"))
            if want is not None and want != self.text[b:e]:
                bad.append(f"{n['@id']}: {want[:40]!r} vs {self.text[b:e][:40]!r}")
        return bad

    def has_row(self, want):
        want = {norm(k): norm(v) for k, v in want.items()}
        return any(all(row.get(k) == v for k, v in want.items()) for row in self.rows.values())

    def nearest_rows(self, want, n=2):
        want = {norm(k): norm(v) for k, v in want.items()}
        score = lambda row: sum(row.get(k) == v for k, v in want.items())
        return sorted(self.rows.values(), key=score, reverse=True)[:n]


def run_check(check, reading):
    """(passed, detail) for one check."""
    if "row" in check:
        ok = reading.has_row(check["row"])
        return ok, None if ok else f"nearest rows: {reading.nearest_rows(check['row'])}"
    if "no_row" in check:
        ok = not reading.has_row(check["no_row"])
        return ok, None if ok else "the row is there"
    if "cell" in check:
        # A cell with each of these: its value and, where given, the names
        # it carries.
        keys = {"value": "doc:cellValue", "column": "doc:columnHeader", "row": "doc:rowHeader", "section": "doc:sectionLabel"}
        want = {keys[k]: norm(v) for k, v in check["cell"].items()}
        ok = any(all(norm(c.get(k, "")) == v for k, v in want.items()) for c in reading.cells)
        return ok, None if ok else "no such cell"
    if "columns" in check:
        # The table holding a cell of this value has exactly this many
        # columns: its cells' `doc:columnIndex` run 0..count-1.
        want = check["columns"]
        found = []
        for t in (n for n in reading.nodes.values() if n.get("@type") == "doco:Table"):
            cells = [reading.nodes.get(c, {}) for c in t.get("po:contains", [])]
            cells = [c for c in cells if c.get("@type") == "doc:TableCell"]
            if any(norm(c.get("doc:cellValue", "")) == norm(want["with"]) for c in cells):
                found.append(sorted({c["doc:columnIndex"] for c in cells}))
        ok = any(idx == list(range(want["count"])) for idx in found)
        return ok, None if ok else f"column indexes {found}"
    if "running" in check:
        # A line read as running text, exactly `count` times where one is
        # given: `0` for a body line that must not be taken.
        hits = reading.running.count(norm(check["running"]))
        ok = hits == check["count"] if "count" in check else hits > 0
        return ok, None if ok else f"running text: {reading.running}"
    if "text" in check:
        # Where a count is given, exactly that many times: a line kept once
        # fails as surely by coming back on every page as by going.
        hits = norm(reading.text).count(norm(check["text"]))
        ok = hits == check["count"] if "count" in check else hits > 0
        return ok, None if ok else f"{hits} times in the text projection"
    if "no_text" in check:
        hits = norm(reading.text).count(norm(check["no_text"]))
        return hits == 0, None if hits == 0 else f"{hits} times in the text projection"
    if "element" in check:
        # An element of this type with exactly this string, line breaks and
        # all, and where given, this evidence: what the text projection's
        # runs of whitespace cannot tell apart, a heading from the line under
        # it, or a header from the body.
        # Where a count is given, exactly that many: zero for a reading that
        # must be gone, such as a link cut short at a line break.
        want = check["element"]
        hits = sum(
            n.get("@type") == want.get("type", n.get("@type"))
            and n.get("nif:isString", "") == want.get("text", n.get("nif:isString", ""))
            and n.get("doc:evidence") == want.get("evidence", n.get("doc:evidence"))
            for n in reading.nodes.values()
        )
        ok = hits == check["count"] if "count" in check else hits > 0
        return ok, None if ok else f"{hits} such elements"
    if "list" in check:
        # One list holding all these items, by their strings: a list carried
        # on past what the page sets between its items, not parted in two.
        want = check["list"]["holds"]
        ok = any(all(w in items for w in want) for items in reading.lists)
        if ok:
            return True, None
        parts = [items for items in reading.lists if any(w in items for w in want)]
        return False, f"in {len(parts)} lists"
    if "nested" in check:
        # A list nested in an item whose string begins with `under`, holding
        # all these items: a sub-list set under its parent's text.
        want = check["nested"]
        lists = [
            items
            for item, subs in reading.nested.items()
            if item.startswith(want["under"])
            for items in subs
        ]
        ok = any(all(w in items for w in want["holds"]) for items in lists)
        return ok, None if ok else f"{len(lists)} lists nested under it"
    if "no_column_header" in check:
        # Anywhere in the name: under a stacked header a row read as header
        # is one label in a path (`A / B / C`).
        name = norm(check["no_column_header"])
        hits = [c["doc:columnHeader"] for c in reading.cells if name in norm(c.get("doc:columnHeader", ""))]
        return not hits, None if not hits else f"{len(hits)} cells, e.g. {hits[0]!r}"
    if "section" in check:
        # A section headed by exactly this title and, where given, at this
        # level and directly inside the section with this title: the outline
        # a document shows, which no single element states.
        want = check["section"]
        ok = any(
            t == want["title"]
            and level == want.get("level", level)
            and within == want.get("within", within)
            for t, level, within in reading.sections
        )
        if ok:
            return True, None
        near = [s for s in reading.sections if s[0] == want["title"]]
        return False, f"as (title, level, within): {near[:2]}" if near else "no section has this title"
    if "under" in check:
        # An element with exactly this string in the section with this
        # title: what a reader takes the text to be about. A table cell is
        # matched by its value, and is in its table's section.
        want = check["under"]
        found = [
            reading.section_of(i)
            for i, n in reading.nodes.items()
            if n.get("nif:isString", n.get("doc:cellValue")) == want["text"]
            and n.get("@type") != "doco:SectionTitle"
        ]
        ok = want["section"] in found
        return ok, None if ok else f"in sections {found[:3]}" if found else "no element has this string"
    if "folio" in check:
        # The page, by its place in the file from 1, carries this printed
        # number, or none where `value` is null: what a reader reassembling
        # the pages goes by.
        want = check["folio"]
        got = reading.folios.get(want["page"])
        return got == want["value"], None if got == want["value"] else f"folio {got!r}"
    if "no_section" in check:
        # No section headed by a title that begins with this: a line that
        # must stay text, wherever and however often it occurs.
        hits = [s[0] for s in reading.sections if (s[0] or "").startswith(check["no_section"])]
        return not hits, None if not hits else f"{len(hits)} sections, e.g. {hits[0]!r}"
    if "same_every_run" in check:
        # The same DoCO, byte for byte, from this many more runs of one
        # binary: what a build gives for a document is all a diff between
        # two builds can rest on.
        runs = [reading.again() for _ in range(check["same_every_run"])]
        differ = sum(r != reading.doco for r in runs)
        return differ == 0, None if differ == 0 else f"{differ} of {len(runs)} runs differ from the first"
    raise SystemExit(f"unknown check kind in {check.get('id')}: {sorted(check)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cases", nargs="*", help="case ids (file stems) to run; default all")
    ap.add_argument("--fdoc", default=str(ROOT / "target" / "release" / "fdoc"))
    ap.add_argument("--fetch", action="store_true", help="download missing public documents")
    ap.add_argument("--dir", action="append", default=[], help="another directory of case files")
    args = ap.parse_args()

    dirs = [HERE / "cases"]
    if (HERE / "private").is_dir():
        dirs.append(HERE / "private")
    dirs += [Path(d) for d in args.dir]
    cases = load_cases(dirs, set(args.cases))
    if not cases:
        raise SystemExit("no cases found")

    failed = missing = opened = fixed = passed = 0
    for case in cases:
        path = document_path(case)
        doc = case["document"]
        if not path.is_file() and args.fetch and "url" in doc:
            fetch(case, path)
        if not path.is_file():
            hint = " (run with --fetch)" if "url" in doc else ""
            print(f"MISSING {case['_id']}: {path}{hint}")
            missing += 1
            continue
        if doc.get("sha256") and sha256(path) != doc["sha256"]:
            print(f"note    {case['_id']}: {path.name} differs from the copy these checks were written on")
        extra = convert_args(case)
        again = lambda: convert(args.fdoc, path, "doco", extra)
        reading = Reading(again(), convert(args.fdoc, path, "text", extra), again)

        results = []
        bad = reading.offsets()
        offsets = {"id": "offsets", "issue": "an element's string is not what its offsets cut from the text"}
        results.append((offsets, not bad, f"{len(bad)} elements, e.g. {bad[:2]}" if bad else None))
        for check in case["checks"]:
            ok, detail = run_check(check, reading)
            results.append((check, ok, detail))

        lines = []
        for check, ok, detail in results:
            is_open = check.get("status") == "open"
            if ok and is_open:
                fixed += 1
                lines.append(f"  NOW PASSES {check['id']}: mark it fixed")
            elif ok:
                passed += 1
            elif is_open:
                opened += 1
                lines.append(f"  open       {check['id']}")
            else:
                failed += 1
                lines.append(f"  FAIL       {check['id']}: {check.get('issue', '')}")
                if detail:
                    lines.append(f"             {detail}")
        status = "FAIL" if any(l.startswith("  FAIL") for l in lines) else "ok"
        print(f"{status:7} {case['_id']} ({len(results)} checks)")
        for l in lines:
            print(l)

    print(
        f"\n{passed} passed, {failed} failed, {opened} open, {fixed} open now passing, "
        f"{missing} documents missing"
    )
    sys.exit(1 if failed or missing else 0)


if __name__ == "__main__":
    main()
