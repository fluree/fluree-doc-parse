#!/usr/bin/env python3
"""Read the twins set and hold OCR to the born-digital page.

For each set make_twins.py wrote, fdoc converts the born-digital PDFs (their
text layer, no OCR) and the images (by OCR, `--ocr oar-ocr`), both to
Markdown, and counts on each: the headings set larger that read as headings,
one-word and longer apart, and the short lines at body size that read as
headings though they are not. The born-digital count is what the image is
held to.

  eval/twins/check.py --models DIR                        # every set, about four minutes at -j4
  eval/twins/check.py --models DIR --sets plain-1.2 fax   # some sets
  eval/twins/check.py --models DIR --baseline eval/twins/baseline.json
  eval/twins/check.py --models DIR --write-baseline eval/twins/baseline.json

`--baseline` fails on a set whose images read fewer headings, or more short
lines as headings, than it records.
"""
import argparse, json, os, re, subprocess, sys, tempfile, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
KEYS = ("long", "word", "false")


def norm(s):
    return re.sub(r"[^a-z0-9]+", "", s.lower())


def headings(md, mark):
    """The normalised headings of a Markdown file; a note mark after a line
    (`Payments1`) is not part of its text."""
    out = set()
    for line in md.splitlines():
        if line.startswith("#"):
            t = re.sub(r"^#+\s*", "", line)
            if mark:
                t = re.sub(r"\s?1$", "", t)
            out.add(norm(t))
    return out


def count(meta, mds, mark):
    """Headings read, one-word and long, and short lines read as headings,
    over the pages whose Markdown `mds` gives."""
    c = dict.fromkeys(KEYS, 0)
    t = dict.fromkeys(KEYS, 0)
    for page, md in mds:
        hs = headings(md, mark)
        for h in page["headings"]:
            k = "word" if " " not in h else "long"
            t[k] += 1
            c[k] += norm(h) in hs
        for s in page["short"]:
            t["false"] += 1
            c["false"] += norm(s) in hs
    return c, t


def unconfigured(fdoc, env, cfg):
    """Exit unless fdoc, run in `cfg` with this environment, reads no
    configuration (see eval/scanned/check.py)."""
    probe = subprocess.run([fdoc, "config", "path"], env=env, cwd=cfg, capture_output=True, text=True,
                           check=False)
    path = Path(probe.stdout.strip())
    if probe.returncode != 0 or path.resolve() != Path(cfg).resolve() / "config.toml" or path.exists():
        sys.exit(f"fdoc run in the empty directory {cfg} would still read a configuration: "
                 f"{probe.stdout.strip() or probe.stderr.strip()}")


def convert(fdoc, files, out, jobs, models):
    cmd = [fdoc, "convert", *map(str, files), "--out-dir", str(out), "-f", "md", "--no-escalate",
           "-j", str(jobs)]
    if models:
        cmd += ["--ocr", "oar-ocr", "--models", str(models)]
    env = {k: v for k, v in os.environ.items() if not k.startswith("FDOC_")}
    with tempfile.TemporaryDirectory() as cfg:
        env["FDOC_HOME"] = cfg
        unconfigured(fdoc, env, cfg)
        run = subprocess.run(cmd, env=env, cwd=cfg, capture_output=True, text=True, check=False)
    if run.returncode != 0:
        sys.exit(f"fdoc convert failed ({run.returncode}):\n{run.stderr[-2000:]}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", type=Path, default=HERE / "corpus")
    ap.add_argument("--fdoc", default=os.environ.get("FDOC_BIN", str(ROOT / "target/release/fdoc")))
    ap.add_argument("--models", default=os.environ.get("FDOC_OCR_MODELS"))
    ap.add_argument("--sets", nargs="*")
    ap.add_argument("-j", "--jobs", type=int, default=4)
    ap.add_argument("--baseline", type=Path)
    ap.add_argument("--write-baseline", type=Path)
    a = ap.parse_args()
    if not a.models:
        sys.exit("--models (or FDOC_OCR_MODELS): the OCR assets directory")
    sets = a.sets or sorted(p.name for p in a.corpus.iterdir() if (p / "meta.json").exists())
    results, rows, t0 = {}, [], time.monotonic()
    born_done = {}
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        for name in sets:
            meta = json.loads((a.corpus / name / "meta.json").read_text())
            twin = meta.get("twin", name)
            mark = name == "superscript"
            if twin not in born_done:
                out = tmp / f"born-{twin}"
                out.mkdir()
                convert(a.fdoc, sorted((a.corpus / twin).glob("*.pdf")), out, a.jobs, None)
                twin_meta = json.loads((a.corpus / twin / "meta.json").read_text())
                born_done[twin] = count(twin_meta["pages"],
                                        [(p, (out / f"{p['page']}.md").read_text()) for p in twin_meta["pages"]],
                                        mark)
            out = tmp / f"ocr-{name}"
            out.mkdir()
            convert(a.fdoc, sorted((a.corpus / name).glob("*.png")), out, a.jobs, a.models)
            dpis = sorted({d for p in meta["pages"] for d in p["dpis"]})
            for dpi in dpis:
                c, t = count(meta["pages"], [(p, (out / f"{p['page']}-{dpi}.md").read_text())
                                             for p in meta["pages"] if dpi in p["dpis"]], mark)
                key = f"{name}@{dpi}"
                results[key] = c
                b, _ = born_done[twin]
                rows.append((key, t, b, c))
    print("| set | headings, one word | headings, a line | short lines as headings |")
    print("|---|---|---|---|")
    for key, t, b, c in rows:
        print(f"| {key} | {c['word']} / {t['word']} (born-digital {b['word']}) "
              f"| {c['long']} / {t['long']} (born-digital {b['long']}) "
              f"| {c['false']} / {t['false']} (born-digital {b['false']}) |")
    print(f"\n{len(rows)} runs in {time.monotonic() - t0:.0f} s")
    if a.write_baseline:
        a.write_baseline.write_text(json.dumps(
            {"about": "per set and dpi: headings read by OCR, one-word and longer, and short lines read as headings",
             "results": results}, indent=1, sort_keys=True) + "\n")
        print(f"baseline written to {a.write_baseline}")
    if a.baseline:
        base = json.loads(a.baseline.read_text())["results"]
        fails = []
        for key, c in results.items():
            b = base.get(key)
            if b is None:
                continue
            for k in ("long", "word"):
                if c[k] < b[k]:
                    fails.append(f"{key}: {c[k]} {k} headings read, the baseline {b[k]}")
            if c["false"] > b["false"]:
                fails.append(f"{key}: {c['false']} short lines read as headings, the baseline {b['false']}")
        for f in fails:
            print("FAIL", f)
        print(f"against the baseline: {len(fails)} failures")
        sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
