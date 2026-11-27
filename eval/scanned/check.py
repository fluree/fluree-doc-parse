#!/usr/bin/env python3
"""Score fdoc on the scanned-PDF set: how many gold facts its text holds.

The set is written by `make_scans.py` (one PDF and one `<id>.gold.json` per
document). Each document is converted to DoCO twice, first as shipped with no
reader (`--no-escalate`, no OCR) and then with the built-in OCR engine
(`--ocr oar-ocr`). Its text projection, what `-f text` writes, is rebuilt from
the DoCO nodes' strings at their character offsets, so a page is read once per
run. A fact is found:

* strictly, when its text occurs in the text projection, runs of whitespace
  compared as one space;
* as a whole word, when that occurrence is also not inside a longer word or
  number (`4,203.33` does not count inside `14,203.33`).

The document node gives the pages and regions left unread, and the OCR
readings. The structure is checked by the headings alone: each document's
`doco:SectionTitle` and `doco:Title` nodes, against the headings its gold
lists, word for word on a born-digital page; on a scan, against the
baseline, by which of the gold's are read and which others.

Usage:
    eval/scanned/check.py --models DIR                 # both runs, the table by profile
    eval/scanned/check.py --no-ocr                     # the run without OCR only, no assets needed
    eval/scanned/check.py --models DIR --show letter-1.fax-200   # one document, every miss shown
    eval/scanned/check.py --models DIR --baseline eval/scanned/baseline.json
    eval/scanned/check.py --models DIR --write-baseline eval/scanned/baseline.json

It measures recall only: a fact is credited when its text is found, and text
the engine invents costs nothing. With --baseline it fails when the set's
digest is not the baseline's, unless --allow-digest-mismatch is given.

The binary is target/release/fdoc unless --fdoc or $FDOC_BIN says otherwise;
the assets directory is --models or $FDOC_OCR_MODELS. Both runs use an empty
fdoc configuration, so a configured model or OCR engine does not take part:
fdoc runs in an empty directory that is also its `FDOC_HOME`, and the check
stops before converting if fdoc would still read a `.fdoc/config.toml` in a
directory above it.
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
KINDS = ["name", "id", "amount", "date", "phrase"]


def norm(s):
    return re.sub(r"\s+", " ", unicodedata.normalize("NFC", s)).strip()


def found(fact, text):
    """(strict, whole word) for one fact in a normalised text."""
    if fact not in text:
        return False, False
    pattern = r"(?<![\w.,])" + re.escape(fact) + r"(?![\w]|[.,]\d)"
    return True, re.search(pattern, text) is not None


def unconfigured(fdoc, env, cfg):
    """Exit unless fdoc, run with this environment in `cfg`, reads no
    configuration. It takes the nearest `.fdoc/config.toml` in its working
    directory or any directory above it, up to the root, else the one under
    $FDOC_HOME: an empty temporary directory as both still leaves one above
    $TMPDIR, whose engine or model would take part in the runs. `fdoc config
    path` names the file it would read, and with none it names where one
    would go under $FDOC_HOME."""
    probe = subprocess.run([fdoc, "config", "path"], env=env, cwd=cfg, capture_output=True, text=True,
                           check=False)
    path = Path(probe.stdout.strip())
    expected = Path(cfg).resolve() / "config.toml"
    if probe.returncode != 0 or path.resolve() != expected or path.exists():
        sys.exit(f"fdoc run in the empty directory {cfg} would still read a configuration, and the check "
                 f"needs none: {probe.stdout.strip() or probe.stderr.strip()}\n"
                 "move it, or point TMPDIR at a directory with no .fdoc/ above it")


def convert(fdoc, pdfs, out, jobs, ocr_models):
    cmd = [fdoc, "convert", *map(str, pdfs), "--out-dir", str(out), "-f", "doco", "--no-escalate",
           "-j", str(jobs)]
    if ocr_models:
        cmd += ["--ocr", "oar-ocr", "--models", str(ocr_models)]
    # fdoc runs in an empty directory that is also its $FDOC_HOME, and every
    # path it is given is absolute.
    env = {k: v for k, v in os.environ.items() if not k.startswith("FDOC_")}
    with tempfile.TemporaryDirectory() as cfg:
        env["FDOC_HOME"] = cfg
        unconfigured(fdoc, env, cfg)
        t0 = time.monotonic()
        run = subprocess.run(cmd, env=env, cwd=cfg, capture_output=True, text=True, check=False)
        elapsed = time.monotonic() - t0
    if run.returncode != 0:
        sys.exit(f"fdoc convert -f doco failed ({run.returncode}):\n{run.stderr[-2000:]}")
    return elapsed


HEADINGS = ("doco:SectionTitle", "doco:Title")


def read_doco(jsonld):
    """(text projection, document node, headings) of one DoCO output. The
    text is each node's `nif:isString` at its `nif:beginIndex`, the gaps
    between them left blank: the offsets are characters of `-f text`, and
    only whitespace lies between its elements, so the two compare equal once
    whitespace is."""
    graph = json.loads(jsonld.read_text())
    nodes = graph.get("@graph", [graph])
    chars = []
    for node in nodes:
        if "nif:isString" not in node:
            continue
        begin, end, string = node["nif:beginIndex"], node["nif:endIndex"], node["nif:isString"]
        if len(string) != end - begin:
            sys.exit(f"{jsonld.name}: {node['@id']} spans {begin}..{end} but holds {len(string)} characters")
        chars += [" "] * (end - len(chars))
        chars[begin:end] = string
    document = next(n for n in nodes if n["@type"] == "foaf:Document")
    headings = [norm(n["nif:isString"]) for n in nodes if n["@type"] in HEADINGS]
    return "".join(chars), document, headings


def listed(document, key):
    return document[key]["@value"] if key in document else []


def run(fdoc, docs, corpus, jobs, ocr_models):
    """Convert every document; per document, its facts found, its headings,
    the pages and regions left unread, and the OCR readings."""
    pdfs = [corpus / f"{d['id']}.pdf" for d in docs]
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        seconds = convert(fdoc, pdfs, tmp, jobs, ocr_models)
        out = {}
        for d in docs:
            text, document, headings = read_doco(tmp / f"{d['id']}.jsonld")
            text = norm(text)
            facts = [(f, *found(norm(f["text"]), text)) for f in d["facts"]]
            out[d["id"]] = {"facts": facts, "text": text, "headings": headings,
                            "unread": listed(document, "doc:unreadPages"),
                            "regions": listed(document, "doc:unreadRegions"),
                            "readings": listed(document, "doc:ocrReadings")}
    return out, seconds


def tally(results, docs, select=lambda d, f: True):
    n = strict = whole = 0
    for d in docs:
        for fact, s, w in results[d["id"]]["facts"]:
            if select(d, fact):
                n, strict, whole = n + 1, strict + s, whole + w
    return n, strict, whole


def pct(k, n):
    return f"{k / n:.3f}" if n else "—"


def off_gold(d, count):
    """(missing, extra): how many headings fewer, or more, than the gold."""
    gold = len(d["headings"])
    return max(gold - count, 0), max(count - gold, 0)


def report(docs, before, after):
    """The tables: by profile, by kind of fact, the pasted pages by where the
    fact is, and the headings by profile."""
    runs = [("no OCR", before)] + ([("OCR", after)] if after else [])
    head = "| profile | docs | facts | unread pages | " + " | ".join(
        f"{name} strict | {name} whole-word" for name, _ in runs) + " |"
    print(head)
    print("|" + "---|" * (head.count("|") - 1))
    by_profile = defaultdict(list)
    for d in docs:
        by_profile[d["profile"]].append(d)
    groups = list(by_profile.items()) + [("all scanned", [d for d in docs if not d["text_layer"]]),
                                         ("all", docs)]
    for profile, ds in groups:
        unread = " → ".join(str(sum(len(r[d["id"]]["unread"]) for d in ds)) for _, r in runs)
        cells = []
        for _, r in runs:
            n, s, w = tally(r, ds)
            cells += [pct(s, n), pct(w, n)]
        n = tally(before, ds)[0]
        print(f"| {profile} | {len(ds)} | {n} | {unread} | " + " | ".join(cells) + " |")
    print()
    print("| kind | facts | " + " | ".join(f"{name} strict | {name} whole-word" for name, _ in runs) + " |")
    print("|---|---|" + "---|---|" * len(runs))
    for kind in KINDS:
        def of_kind(d, f, kind=kind):
            return f["kind"] == kind

        cells = []
        for _, r in runs:
            n, s, w = tally(r, docs, of_kind)
            cells += [pct(s, n), pct(w, n)]
        print(f"| {kind} | {tally(before, docs, of_kind)[0]} | " + " | ".join(cells) + " |")
    pasted = [d for d in docs if d["text_layer"]]
    if pasted:
        print()
        print("| pasted pages | facts | " + " | ".join(f"{name} strict" for name, _ in runs) + " |")
        print("|---|---|" + "---|" * len(runs))
        for where, label in [("page", "in the text layer"), ("pixels", "only in the screenshot")]:
            def sel(d, f, where=where):
                return f["where"] == where

            cells = [pct(tally(r, pasted, sel)[1], tally(r, pasted, sel)[0]) for _, r in runs]
            print(f"| {label} | {tally(before, pasted, sel)[0]} | " + " | ".join(cells) + " |")
    print()
    print("| headings | docs | gold | " + " | ".join(f"{name} read | {name} missing | {name} extra | "
                                                  f"{name} docs off | {name} gold read"
                                                  for name, _ in runs) + " |")
    print("|---|---|---|" + "---|---|---|---|---|" * len(runs))
    for profile, ds in groups:
        cells = []
        for _, r in runs:
            offs = [off_gold(d, len(r[d["id"]]["headings"])) for d in ds]
            cells += [str(sum(len(r[d["id"]]["headings"]) for d in ds)), str(sum(m for m, _ in offs)),
                      str(sum(e for _, e in offs)), str(sum(1 for m, e in offs if m or e)),
                      str(sum(len(gold_read(d, r[d["id"]]["headings"])) for d in ds))]
        gold = sum(len(d["headings"]) for d in ds)
        print(f"| {profile} | {len(ds)} | {gold} | " + " | ".join(cells) + " |")


def shows(region, box):
    """Whether an unread region is this box, `[x0, y0, x1, y1]` in points
    from the top left, to within a point."""
    got = [float(v) for v in region["bbox"].split(",")]
    return region["pageIndex"] == 0 and all(abs(a - b) <= 1 for a, b in zip(got, box))


def gate(d, before, after):
    """What is wrong with one document's routing, without a reader and with
    OCR (`after`, None when it did not run)."""
    out = []
    # Without a reader, a scanned page is a page nothing read, for being a
    # scan; a pasted page's text is read, and its screenshot is a region
    # nothing read. No fact only pixels hold may reach the text: that would
    # mean the set or the router lies.
    if not d["text_layer"] and [u["reason"] for u in before["unread"]] != ["Scanned"]:
        out.append(f"without OCR, not one page in doc:unreadPages as Scanned: {before['unread']}")
    if d["text_layer"]:
        if before["unread"]:
            out.append(f"a born-digital page is in doc:unreadPages: {before['unread']}")
        if not any(shows(r, d["screenshot"]) for r in before["regions"]):
            out.append(f"without OCR, the screenshot {d['screenshot']} is not in doc:unreadRegions: "
                       f"{before['regions']}")
    # A born-digital page's headings are the gold's, word for word: the gold
    # lists what fdoc makes headings of on such a page, and a screenshot adds
    # none. Their order is not compared: the gold lists them as drawn, not in
    # reading order.
    if d["text_layer"] and sorted(before["headings"]) != sorted(map(norm, d["headings"])):
        out.append(f"a born-digital page reads the headings {before['headings']}, the gold "
                   f"{d['headings']}")
    leaked = [f["text"] for f, s, _ in before["facts"] if s and (not d["text_layer"] or f["where"] == "pixels")]
    if leaked:
        out.append(f"read without a reader: {leaked}")
    if after:
        # With OCR nothing is left unread, and a page with a text layer has
        # its screenshot read, not the whole page: OCR adds to that text, it
        # does not replace it.
        if after["unread"] or after["regions"]:
            out.append(f"left unread with OCR: {after['unread'] + after['regions']}")
        if d["text_layer"] and any(r["crop"] == "full" for r in after["readings"]):
            out.append(f"OCR read the whole of a born-digital page: {after['readings']}")
    return out


def corpus_digest(corpus, docs):
    """One SHA-256 over every PDF and gold file of the set, in index order."""
    h = hashlib.sha256()
    for d in docs:
        for suffix in (".pdf", ".gold.json"):
            h.update((corpus / f"{d['id']}{suffix}").read_bytes())
    return h.hexdigest()


def misses(results):
    return {doc: sorted(f["text"] for f, s, w in r["facts"] if not w) for doc, r in sorted(results.items())}


def heading_counts(results):
    return {doc: len(r["headings"]) for doc, r in sorted(results.items())}


def gold_read(d, headings):
    """The gold's headings among those read, normalised and sorted."""
    return sorted(set(map(norm, d["headings"])) & set(headings))


def gold_headings_read(docs, results):
    return {d["id"]: gold_read(d, results[d["id"]]["headings"]) for d in sorted(docs, key=lambda d: d["id"])}


def other_read(d, headings):
    """The headings read that are not the gold's, normalised and sorted, as
    often as read: a heading of the gold read twice is the gold's once and
    one of these once."""
    return sorted((Counter(headings) - Counter(set(map(norm, d["headings"])))).elements())


def other_headings_read(docs, results):
    return {d["id"]: other_read(d, results[d["id"]]["headings"]) for d in sorted(docs, key=lambda d: d["id"])}


def gold_headings_lost(docs, now, base):
    """The documents that no longer read as a heading one of the gold's
    headings the baseline read as one. A count alone would not see a real
    heading lost and a false one gained; a document the baseline does not
    list fails apart (see `main`)."""
    out = []
    for d in docs:
        if d["id"] not in base:
            continue
        headings = now[d["id"]]["headings"]
        lost = sorted(set(base[d["id"]]) - set(gold_read(d, headings)))
        if lost:
            out.append(f"{d['id']}: the gold's {lost} no longer read as headings: {headings}")
    return out


def other_headings_gained(docs, now, base):
    """The documents that read as a heading a line the gold does not set apart
    and the baseline did not read as one, or read one more often. Most scans
    read fewer headings than their gold, so a body line turned into a heading
    brings the count nearer the gold's while it is a false heading; a document
    the baseline does not list fails apart (see `main`)."""
    out = []
    for d in docs:
        if d["id"] not in base:
            continue
        headings = now[d["id"]]["headings"]
        gained = sorted((Counter(other_read(d, headings)) - Counter(base[d["id"]])).elements())
        if gained:
            out.append(f"{d['id']}: {gained} read as headings, not the gold's nor the baseline's: {headings}")
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--corpus", type=Path, default=HERE / "corpus")
    ap.add_argument("--fdoc", default=os.environ.get("FDOC_BIN", str(ROOT / "target/release/fdoc")))
    ap.add_argument("--models", default=os.environ.get("FDOC_OCR_MODELS"))
    ap.add_argument("--no-ocr", action="store_true", help="only the run without OCR")
    ap.add_argument("-j", "--jobs", type=int, default=4)
    ap.add_argument("--show", metavar="ID", help="one document: its text and every fact missed")
    ap.add_argument("--baseline", type=Path,
                    help="fail when a fact found in this baseline is missed now, a heading of the gold read "
                         "then is not, a heading other than the gold's is read that was not, a document "
                         "is not in it or is not in the set, or it lacks one of its lists")
    ap.add_argument("--write-baseline", type=Path)
    ap.add_argument("--allow-digest-mismatch", action="store_true",
                    help="with --baseline, warn rather than fail when the set is not the one the baseline "
                         "was written on (another platform's bytes)")
    args = ap.parse_args()
    if args.no_ocr and (args.baseline or args.write_baseline):
        ap.error("--baseline and --write-baseline are about the OCR run; drop --no-ocr")
    if args.baseline and args.write_baseline and args.baseline.resolve() == args.write_baseline.resolve():
        # Written first, the baseline would be compared with itself and a lost
        # fact recorded as if it had never been found.
        ap.error("--baseline and --write-baseline name the same file: check against it first, then write it")
    # fdoc runs in an empty directory (see `convert`), so a path given relative
    # to here must be made absolute first.
    args.corpus = args.corpus.resolve()
    if os.sep in args.fdoc:
        args.fdoc = str(Path(args.fdoc).resolve())
    if args.models:
        args.models = str(Path(args.models).resolve())

    index = args.corpus / "index.json"
    if not index.exists():
        sys.exit(f"{index} is missing; write the set with `uv run eval/scanned/make_scans.py`")
    ids = json.loads(index.read_text())["documents"]
    if args.show:
        ids = [i for i in ids if i == args.show]
        if not ids:
            sys.exit(f"no document {args.show} in {args.corpus}")
    docs = [json.loads((args.corpus / f"{i}.gold.json").read_text()) for i in ids]
    if not args.no_ocr and not args.models:
        sys.exit("the OCR run needs the assets directory: --models DIR or FDOC_OCR_MODELS (or pass --no-ocr)")

    before, t_before = run(args.fdoc, docs, args.corpus, args.jobs, None)
    after, t_after = (None, 0.0) if args.no_ocr else run(args.fdoc, docs, args.corpus, args.jobs, args.models)

    if args.show:
        r = (after or before)[args.show]
        print(r["text"], end="\n\n")
        for fact, s, w in r["facts"]:
            if not w:
                print(f"MISSED ({'strict only' if s else 'not found'}) [{fact['kind']}] {fact['text']!r}")
        print(f"headings: {r['headings']}; the gold's: {docs[0]['headings']}")
        print(f"unread pages: {r['unread']}; unread regions: {r['regions']}; OCR readings: {r['readings']}")
        return 0

    report(docs, before, after)
    pages = len(docs)
    print(f"\n{pages} pages; conversion without OCR {t_before:.1f} s"
          + ("" if args.no_ocr else f", with OCR {t_after:.1f} s ({t_after / pages:.2f} s/page at -j{args.jobs})"))

    rc = 0
    for d in docs:
        for failure in gate(d, before[d["id"]], after and after[d["id"]]):
            print(f"FAIL {d['id']}: {failure}")
            rc = 1

    digest = corpus_digest(args.corpus, docs)
    print(f"set digest {digest}")
    if after and args.write_baseline:
        args.write_baseline.write_text(json.dumps(
            {"about": "facts each document misses as a whole word with --ocr oar-ocr, how many "
                      "headings it reads, which of its gold's, and which others",
             "corpus_sha256": digest, "gold_headings_read": gold_headings_read(docs, after),
             "other_headings_read": other_headings_read(docs, after),
             "missed": misses(after), "headings": heading_counts(after)},
            indent=1, ensure_ascii=False) + "\n")
        print(f"baseline written to {args.write_baseline}")
    if after and args.baseline:
        baseline = json.loads(args.baseline.read_text())
        # Another set's pixels read differently: a fact lost, or one found,
        # may be the pixels rather than fdoc, so a mismatch fails unless the
        # caller says the comparison is still wanted (another platform).
        if baseline.get("corpus_sha256") != digest:
            print(f"{'WARNING' if args.allow_digest_mismatch else 'DIGEST MISMATCH'}: this set is not the "
                  f"one the baseline was written on ({baseline.get('corpus_sha256')}): another seed, other "
                  "versions of the generator's dependencies, or another platform (the bytes hold on one "
                  "machine, its interpreter's zlib and its wheels); a lost fact may be the pixels, not fdoc"
                  + ("" if args.allow_digest_mismatch else ". Pass --allow-digest-mismatch to compare anyway"))
            if not args.allow_digest_mismatch:
                rc = 1
        # Each gate reads one of these lists: a baseline without one would pass
        # that gate unchecked, so it fails before any is run.
        listing = ("missed", "gold_headings_read", "other_headings_read")
        incomplete = [k for k in listing if not isinstance(baseline.get(k), dict)]
        if incomplete:
            print(f"BASELINE INCOMPLETE: {args.baseline} has no {', '.join(incomplete)}; write it again")
            return 1
        # A document the baseline does not list has nothing to be held to, and
        # would pass the headings gates unchecked; one it lists that the set no
        # longer holds is a document the gates stopped checking. Either fails,
        # so that the set and its baseline change together.
        for d in docs:
            absent = [k for k in listing if d["id"] not in baseline[k]]
            if absent:
                print(f"NOT IN THE BASELINE {d['id']}: no entry in {', '.join(absent)}; write it again")
                rc = 1
        ids = {d["id"] for d in docs}
        for doc in sorted({doc for k in listing for doc in baseline[k]} - ids):
            listed = [k for k in listing if doc in baseline[k]]
            print(f"NOT IN THE SET {doc}: listed in the baseline's {', '.join(listed)}; write it again")
            rc = 1
        base = baseline["missed"]
        now = misses(after)
        lost = gained = 0
        for doc, missed in now.items():
            if doc not in base:
                continue
            was = set(base[doc])
            for fact in sorted(set(missed) - was):
                print(f"LOST {doc}: {fact!r}")
                lost += 1
            gained += len(was - set(missed))
        print(f"against the baseline: {lost} facts lost, {gained} gained")
        if lost:
            rc = 1
        # The headings are gated on both sides: none of the gold's read then
        # may be lost, and none other may be gained. A count alone would take
        # a false heading on a page that lacks some for progress.
        for key, check, what in [("gold_headings_read", gold_headings_lost, "lost one of the gold's"),
                                 ("other_headings_read", other_headings_gained, "gained one not the gold's")]:
            failures = check(docs, after, baseline[key])
            for line in failures:
                print(f"HEADINGS {line}")
            print(f"headings against the baseline: {len(failures)} documents {what}")
            if failures:
                rc = 1
        gold, other = baseline["gold_headings_read"], baseline["other_headings_read"]
        better = sum(1 for d in docs if d["id"] in gold and d["id"] in other
                     and (len(gold_read(d, after[d["id"]]["headings"])) > len(gold[d["id"]])
                          or len(other_read(d, after[d["id"]]["headings"])) < len(other[d["id"]])))
        print(f"headings against the baseline: {better} documents read more of the gold's or fewer others")
    return rc


if __name__ == "__main__":
    sys.exit(main())
