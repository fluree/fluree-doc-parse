"""Build the two glyph-layer indexes every score is measured against.

`pagelines` is the page, used for precision: a value a reader emits that
appears nowhere on the page was not read, it was produced.

`regionlines` is the page sliced to one table's rectangle, used for recall:
the values printed inside that rectangle are knowable without consulting
any reader, so a reader can be told what it missed rather than only what it
emitted. Row counts alone cannot distinguish a reader that split a merged
row from one that invented one.

    python3 groundtruth.py
"""
from common import (PAGELINES, REGIONLINES, REGIONS, key, lines_of, load,
                    page_glyphs, save)

if __name__ == "__main__":
    regions = load(REGIONS)
    pages, plines = {}, load(PAGELINES)
    rlines = load(REGIONLINES)

    todo = sorted({(r["doc"], r["page"]) for r in regions if key(r["doc"], r["page"]) not in plines}
                  | {(r["doc"], r["page"]) for r in regions if r["name"] not in rlines})
    for i, (doc, page) in enumerate(todo):
        pages[(doc, page)] = page_glyphs(doc, page)
        plines[key(doc, page)] = lines_of(pages[(doc, page)])
        print(f"  {i+1}/{len(todo)} {doc} p{page}: {len(pages[(doc, page)])} glyphs", flush=True)

    for r in regions:
        if r["name"] in rlines:
            continue
        gs = pages.get((r["doc"], r["page"]))
        if gs is None:
            gs = pages[(r["doc"], r["page"])] = page_glyphs(r["doc"], r["page"])
        b, pad = r["bbox"], 2.0
        rlines[r["name"]] = lines_of([g for g in gs
                                      if b["x0"] - pad <= g[1] <= b["x1"] + pad
                                      and b["y0"] - pad <= g[0] <= b["y1"] + pad])

    save(plines, PAGELINES)
    save(rlines, REGIONLINES)
    print(f"wrote {len(plines)} pages, {len(rlines)} regions")
