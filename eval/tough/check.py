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


def convert(fdoc, path, fmt):
    out = subprocess.run(
        [fdoc, "convert", str(path), "--no-escalate", "-f", fmt],
        capture_output=True,
        check=True,
    ).stdout
    return out.decode("utf-8")


class Reading:
    """A document's DoCO graph and text projection, indexed for the checks."""

    def __init__(self, graph, text):
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
    if "text" in check:
        ok = norm(check["text"]) in norm(reading.text)
        return ok, None if ok else "not in the text projection"
    if "no_text" in check:
        hits = norm(reading.text).count(norm(check["no_text"]))
        return hits == 0, None if hits == 0 else f"{hits} times in the text projection"
    if "no_column_header" in check:
        # Anywhere in the name: under a stacked header a row read as header
        # is one label in a path (`A / B / C`).
        name = norm(check["no_column_header"])
        hits = [c["doc:columnHeader"] for c in reading.cells if name in norm(c.get("doc:columnHeader", ""))]
        return not hits, None if not hits else f"{len(hits)} cells, e.g. {hits[0]!r}"
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
        reading = Reading(json.loads(convert(args.fdoc, path, "doco"))["@graph"], convert(args.fdoc, path, "text"))

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
