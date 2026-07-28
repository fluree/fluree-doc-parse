"""Build the region manifest: every table on a page the trigger flagged.

The population under test is deliberately not "all tables" -- it is the
tables `table::suspect_tables` reports as Fragmented or MergedRows, because
those are the only ones an escalation tier would ever see. Measuring a
model on tables we already read correctly would flatter it against a
baseline that never fires.

    python3 regions.py [doc ...]
"""
import json, os, sys

from common import CORPUS, REGIONS, fdoc, save


def flagged_pages():
    """{doc: {page, ...}} from `fdoc triage`, the trigger itself."""
    out = {}
    for ln in fdoc("triage", CORPUS).splitlines():
        parts = ln.split("\t")
        if len(parts) < 3 or parts[1] != "TABLE":
            continue
        pages = set()
        for s in parts[2].split(";"):
            s = s.strip()
            if s.startswith("p"):
                pages.add(int(s[1:].split()[0]))
        if pages:
            out[parts[0]] = pages
    return out


def tables_on(doc, page):
    """Every doco:Table our own reader emits for that page."""
    txt = fdoc("convert", os.path.join(CORPUS, doc + ".pdf"), "-f", "json",
               "--pages", str(page + 1))
    try:
        els = json.loads(txt)
    except Exception:
        return []
    return [e for e in els if e.get("type") == "doco:Table" and e.get("bbox")]


if __name__ == "__main__":
    only = set(sys.argv[1:])
    regions = []
    for doc, pages in sorted(flagged_pages().items()):
        if only and doc not in only:
            continue
        for page in sorted(pages):
            for e in tables_on(doc, page):
                cells = e.get("cells") or []
                # `elem-00276` is the 1-based id of a 0-based element index;
                # the name carries the index so a crop can be traced back to
                # the element that produced it.
                idx = int(e["id"].split("-")[-1]) - 1
                regions.append({
                    "name": f"{doc}_p{page}_t{idx}", "idx": idx,
                    "doc": doc, "page": page, "bbox": e["bbox"],
                    "rows": len(cells), "cols": max((len(r) for r in cells), default=0),
                    "cells": cells,
                })
        print(f"  {doc}: {len(pages)} flagged pages", flush=True)
    save(regions, REGIONS)
    print(f"wrote {REGIONS}: {len(regions)} tables on "
          f"{len({(r['doc'], r['page']) for r in regions})} pages")
