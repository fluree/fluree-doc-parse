#!/usr/bin/env python3
"""Check fdoc's table cells against the cells each document means.

Each document `<stem>.<format>` has a `<stem>.expect.json` beside it: the
document's blocks in reading order (text, and tables with every column's
expected header path) and the parts of it that must be found. A part is a
span of a text block, a body cell, a header label, or a section band. A
document is *kept* when every part marked `needed` is found where the
document puts it:

* a text part inside the block's text in `fdoc convert -f text`, the block
  found in reading order, and the part inside one element's span;
* a cell part in a `doc:TableCell` whose value is the cell's (or the same
  number), whose `doc:columnHeader`, `doc:rowHeader` and `doc:sectionLabel`
  are the ones the table gives it, and whose offsets slice the text to it;
* a header part on its header line of the table's text, in its column;
* a band part on a line of its own inside the table's text.

Usage:
    eval/tables/check.py                       # the tracked fixtures
    eval/tables/check.py eval/tables-full      # a whole corpus directory
    eval/tables/check.py eval/tables/fixtures/w1-d029.pdf   # one file, gaps shown
    eval/tables/check.py --baseline eval/tables/baseline.json   # fail on any regression
    eval/tables/check.py --write-baseline eval/tables/baseline.json

The binary is target/release/fdoc unless --fdoc or $FDOC_BIN says otherwise.
"""
import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor
from decimal import Decimal, InvalidOperation
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FORMATS = ["html", "md", "docx", "xlsx", "pdf"]
CONTAINERS = {"doco:Table", "doco:Section", "doco:BodyMatter", "doco:Document", "doco:List"}
HEADERS = ("doc:columnHeader", "doc:rowHeader", "doc:sectionLabel")


def norm(s):
    if s is None:
        return None
    s = re.sub(r"\s+", " ", s).strip()
    return s or None


def num_of(s):
    """(the text around the number, the number): '£1,450.00' -> ('£#', 1450.00)."""
    m = re.search(r"-?\d[\d,]*(?:\.\d+)?", s)
    if not m:
        return None
    try:
        return (s[: m.start()] + "#" + s[m.end():]).strip(), Decimal(m.group(0).replace(",", ""))
    except InvalidOperation:
        return None


def ws(s):
    return "".join(r"\s+" if p.isspace() else re.escape(p) for p in re.findall(r"\s+|[^\s]+", s))


def flexible(text, marks):
    """A pattern for `text` whose whitespace runs match any whitespace, with a
    named group per marked span."""
    out, i = [], 0
    for b, e, name in sorted(marks):
        out.append(ws(text[i:b]))
        out.append(f"(?P<{name}>{ws(text[b:e])})")
        i = e
    out.append(ws(text[i:]))
    return re.compile("".join(out))


def parts_of(expect):
    """The expectation's parts; a table marked `all_cells` needs every
    non-empty body cell, whole."""
    parts = list(expect.get("parts", []))
    for bi, block in enumerate(expect["blocks"]):
        t = block.get("table")
        if not (t and t.get("all_cells")):
            continue
        for r, row in enumerate(t["rows"]):
            for c, value in enumerate(row.get("cells", [])):
                if value.strip():
                    parts.append({"block": bi, "kind": "cell", "row": r, "col": c, "start": 0,
                                  "surface": value, "needed": True})
    return parts


def align(expect, text, graph):
    """Find each part in fdoc's text and cells: (located part indices, gaps)."""
    blocks = expect["blocks"]
    parts = parts_of(expect)
    cells = [n for n in graph if n.get("@type") == "doc:TableCell"]
    placed = [n for n in cells if "nif:beginIndex" in n]
    tables = [n for n in graph if n.get("@type") == "doco:Table" and "nif:beginIndex" in n]
    leaves = [n for n in graph if "nif:beginIndex" in n and n.get("@type") not in CONTAINERS]
    by_block = defaultdict(list)
    for i, p in enumerate(parts):
        by_block[p["block"]].append(i)
    loc, gaps, used = {}, [], set()

    def gap(kind, i, **kw):
        p = parts[i]
        gaps.append({"kind": kind, "surface": p["surface"], "needed": p.get("needed", True), **kw})

    def smallest(b, e):
        inside = [n for n in leaves if n["nif:beginIndex"] <= b and e <= n["nif:endIndex"]]
        return min(inside, key=lambda n: n["nif:endIndex"] - n["nif:beginIndex"], default=None)

    def table_at(b):
        return next((t for t in tables if t["nif:beginIndex"] <= b < t["nif:endIndex"]), None)

    cursor = 0
    table_of_block = {}
    for bi, block in enumerate(blocks):
        pids = by_block.get(bi, [])
        if "text" in block:
            marks = [(parts[i]["start"], parts[i]["start"] + len(parts[i]["surface"]), f"p{i}") for i in pids]
            pat = flexible(block["text"], marks)
            m = pat.search(text, cursor) or pat.search(text)
            if not m:
                for i in pids:
                    gap("text-not-found", i, block_text=block["text"])
                continue
            cursor = m.end()
            for i in pids:
                b, e = m.span(f"p{i}")
                if smallest(b, e) is None:
                    gap("outside-elements", i)
                else:
                    loc[i] = (b, e)
            continue

        t = block["table"]
        heads = t["columns"]
        section = None
        first_cell = None
        for r, row in enumerate(t["rows"]):
            if "section" in row:
                section = row["section"]
                continue
            row_header = norm(row["cells"][0]) if row["cells"] else None
            for c, value in enumerate(row["cells"]):
                if not value.strip():
                    continue
                want = {"doc:columnHeader": norm(heads[c]) if c < len(heads) else None,
                        "doc:rowHeader": row_header if c > 0 else None,
                        "doc:sectionLabel": section}
                cpids = [i for i in pids if parts[i]["kind"] == "cell"
                         and parts[i]["row"] == r and parts[i]["col"] == c]
                same_value, hit = [], None
                for n in placed:
                    if n["@id"] in used:
                        continue
                    exact = norm(n.get("doc:cellValue")) == norm(value)
                    numeric = not exact and num_of(value) is not None and num_of(value) == num_of(n.get("doc:cellValue", ""))
                    if not (exact or numeric):
                        continue
                    same_value.append(n)
                    if all(norm(n.get(k)) == v for k, v in want.items()):
                        hit = (n, exact)
                        break
                if hit is None:
                    unplaced = any("nif:beginIndex" not in n and norm(n.get("doc:cellValue")) == norm(value) for n in cells)
                    kind = "wrong-headers" if same_value else "cell-without-offsets" if unplaced else "no-cell"
                    found = [{k: n.get(k) for k in ("doc:cellValue",) + HEADERS} for n in same_value[:2]]
                    for i in cpids:
                        gap(kind, i, cell=[r, c], want=want, found=found)
                    continue
                n, exact = hit
                used.add(n["@id"])
                first_cell = first_cell or n
                b0, e0 = n["nif:beginIndex"], n["nif:endIndex"]
                slice_ = text[b0:e0]
                if not cpids:
                    continue
                marks = [(parts[i]["start"], parts[i]["start"] + len(parts[i]["surface"]), f"p{i}") for i in cpids]
                m = flexible(value, marks).fullmatch(slice_)
                if m:
                    for i in cpids:
                        b, e = m.span(f"p{i}")
                        loc[i] = (b0 + b, b0 + e)
                elif not exact and len(cpids) == 1:
                    loc[cpids[0]] = (b0, e0)
                else:
                    for i in cpids:
                        gap("part-not-in-cell", i, cell=[r, c], parser=slice_)
        if first_cell is not None:
            table_of_block[bi] = table_at(first_cell["nif:beginIndex"])
            if table_of_block[bi]:
                cursor = max(cursor, table_of_block[bi]["nif:endIndex"])

        # Header labels: on their header line of the table's text.
        grid = t.get("header_grid") or []
        for i in pids:
            p = parts[i]
            if p["kind"] != "header":
                continue
            tb = table_of_block.get(bi)
            if tb is None:
                gap("table-not-found", i)
                continue
            data_begin = min((n["nif:beginIndex"] for n in placed
                              if tb["nif:beginIndex"] <= n["nif:beginIndex"] < tb["nif:endIndex"]),
                             default=tb["nif:endIndex"])
            head_text = text[tb["nif:beginIndex"]:data_begin]
            lines = head_text.split("\n")
            hrow, col = p["hrow"], p["col"]
            label = (grid[hrow][col] if hrow < len(grid) and col < len(grid[hrow]) else None) or ""
            got = None
            if hrow < len(lines):
                fields = lines[hrow].split("\t")
                if not (col < len(fields) and norm(fields[col]) == norm(label)):
                    same = [k for k, f in enumerate(fields) if norm(f) == norm(label)]
                    col = same[0] if len(same) == 1 else None
                if col is not None:
                    off = tb["nif:beginIndex"] + sum(len(x) + 1 for x in lines[:hrow]) + sum(len(x) + 1 for x in fields[:col])
                    b = off + len(fields[col]) - len(fields[col].lstrip()) + p["start"]
                    got = (b, b + len(p["surface"]))
            if got is None or text[got[0]:got[1]] != p["surface"]:
                gap("header-slot-not-found", i, label=label)
                continue
            loc[i] = got

        # Section bands: each on a line of its own in the table's text, the
        # n-th band of that name being the n-th such line.
        bands = sorted((i for i in pids if parts[i]["kind"] == "band"), key=lambda i: parts[i]["row"])
        if bands:
            located_cells = [i for i in pids if parts[i]["kind"] == "cell" and i in loc]
            tb = table_at(loc[located_cells[0]][0]) if located_cells else None
            for i in bands:
                p = parts[i]
                if tb is None:
                    gap("band-table-not-found", i)
                    continue
                b0, e0 = tb["nif:beginIndex"], tb["nif:endIndex"]
                n = sum(1 for row in t["rows"][: p["row"]] if row.get("section") == p["surface"])
                hits = [m.start() for m in re.finditer(r"(?m)^" + re.escape(p["surface"]) + r"$", text[b0:e0])]
                if len(hits) <= n:
                    gap("band-not-found", i)
                    continue
                loc[i] = (b0 + hits[n], b0 + hits[n] + len(p["surface"]))

    kept = all(i in loc for i, p in enumerate(parts) if p.get("needed", True))
    return kept, [g for g in gaps if g["needed"]]


def run_fdoc(fdoc, path, fmt):
    out = []
    for f in ("text", "doco"):
        r = subprocess.run([fdoc, "convert", str(path), "-f", f, "--no-escalate"], capture_output=True, text=True)
        if r.returncode:
            return None
        out.append(r.stdout)
    return out


def check_file(fdoc, path, expect):
    got = run_fdoc(fdoc, path, path.suffix[1:])
    if got is None:
        return False, [{"kind": "parse-failed"}]
    text, doco = got
    try:
        graph = json.loads(doco)["@graph"]
    except (ValueError, KeyError):
        return False, [{"kind": "parse-failed"}]
    return align(expect, text, graph)


def collect(args):
    """(file path, expectation) for every document file the arguments name."""
    out = []
    for a in args:
        p = Path(a)
        if p.is_dir():
            for e in sorted(p.rglob("*.expect.json")):
                out += files_of(e)
        elif p.name.endswith(".expect.json"):
            out += files_of(p)
        else:
            e = p.with_name(p.name.rsplit(".", 1)[0] + ".expect.json")
            if not e.exists():
                sys.exit(f"{p}: no {e.name} beside it")
            out.append((p, json.loads(e.read_text())))
    return out


def files_of(expect_path):
    expect = json.loads(expect_path.read_text())
    stem = expect_path.name[: -len(".expect.json")]
    return [(f, expect) for fmt in expect["formats"] if (f := expect_path.with_name(f"{stem}.{fmt}")).exists()]


def key(path):
    try:
        return str(path.resolve().relative_to(ROOT))
    except ValueError:
        return str(path)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("paths", nargs="*", help="directories, expectation files or document files")
    ap.add_argument("--fdoc", default=os.environ.get("FDOC_BIN", str(ROOT / "target/release/fdoc")))
    ap.add_argument("--baseline", help="fail when a file kept in this baseline is dropped now")
    ap.add_argument("--write-baseline", help="record which files are kept now")
    ap.add_argument("-v", "--verbose", action="store_true", help="every dropped file with its gaps")
    ap.add_argument("-j", "--jobs", type=int, default=os.cpu_count() or 4)
    args = ap.parse_args()

    named_files = bool(args.paths) and all(not Path(a).is_dir() and not a.endswith(".expect.json") for a in args.paths)
    items = collect(args.paths or [str(ROOT / "eval/tables/fixtures")])
    if not items:
        sys.exit("no documents with expectations found")
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(lambda it: check_file(args.fdoc, *it), items))

    kept, total, kinds = Counter(), Counter(), Counter()
    now = {}
    for (path, expect), (ok, gaps) in zip(items, results):
        fam, fmt = expect.get("family", "-"), path.suffix[1:]
        total[(fam, fmt)] += 1
        kept[(fam, fmt)] += ok
        now[key(path)] = ok
        if not ok:
            kinds.update(f"{fmt}/{g['kind']}" for g in gaps)
        if named_files or (args.verbose and not ok):
            print(f"{key(path)} ({fam}): {'kept' if ok else 'DROPPED'}")
            for g in gaps:
                print("  gap", json.dumps({k: v for k, v in g.items() if k != "needed"}, ensure_ascii=False))

    if not named_files:
        fams = list(dict.fromkeys(f for f, _ in total))
        fmts = [f for f in FORMATS if any(total[(x, f)] for x in fams)]
        print("| family | " + " | ".join(fmts) + " |")
        print("| --- |" + " --- |" * len(fmts))
        for fam in fams:
            print(f"| {fam} | " + " | ".join(f"{kept[(fam, f)]}/{total[(fam, f)]}" if total[(fam, f)] else "—" for f in fmts) + " |")
        print("| all | " + " | ".join(f"{sum(kept[(x, f)] for x in fams)}/{sum(total[(x, f)] for x in fams)}" for f in fmts) + " |")
        print(f"kept {sum(kept.values())} of {sum(total.values())}"
              + (f"; gap kinds: {', '.join(f'{k} {v}' for k, v in kinds.most_common(8))}" if kinds else ""))

    status = 0
    if args.baseline:
        base = json.loads(Path(args.baseline).read_text())
        lost = sorted(f for f, ok in base.items() if ok and now.get(f) is False)
        won = sorted(f for f, ok in now.items() if ok and base.get(f) is False)
        for f in lost:
            print(f"REGRESSED {f}")
        for f in won:
            print(f"now kept  {f}")
        print(f"baseline: {len(lost)} regressed, {len(won)} newly kept")
        status = 1 if lost else 0
    if args.write_baseline:
        Path(args.write_baseline).write_text(json.dumps(dict(sorted(now.items())), indent=1) + "\n")
    return status


if __name__ == "__main__":
    sys.exit(main())
