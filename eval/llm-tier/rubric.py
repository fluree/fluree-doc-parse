"""The acceptance rubric: judge a table reading without ground truth.

This is the file that matters beyond the experiment. Every measure here is
computable at runtime from the PDF alone, so the same functions that ranked
seven engines offline can decide, in the pipeline, whether an escalated
reading has earned its place. No reference table is needed -- the page is
the reference.

  fabrication  values the reading emits that are not printed on the page
  recall       values printed in the table that the reading captured
  crammed      columns of single values holding a cell of three or more,
               which is what a merged row looks like once collapsed

Matching is punctuation-tolerant on purpose. A font whose decimal point
maps to nothing puts `200<fffd>52` in the glyph layer where the page prints
200.52; a literal comparison would score that correct reading as invented.
Values are counted only when they carry three or more digits: a bare `1`
sits somewhere on any page and would dilute the measure toward zero.

Whatever judges a model must judge us by the same function, or the
comparison is not a comparison. Our own reading scores ~0% fabricated by
construction -- it can only report glyphs that exist -- and that control is
what makes a model's number trustworthy rather than plausible.
"""
import html as htmlmod
import re

NUM = re.compile(r"\d[\d,]*(?:\.\d+)?")
GNUM = re.compile(r"\d[\d,.·‧�]*\d|\d+")
SEP = r"[\s]*[.,·‧�]?[\s]*"
SPAN = re.compile(r"<t[dh][^>]*\b(?:col|row)span\s*=\s*[\"']?[2-9]", re.I)
_pat_cache = {}


def values(text):
    """Numeric runs carrying 3+ digits, from text that may hold entities."""
    return [t for t in (m.rstrip(",.") for m in NUM.findall(htmlmod.unescape(text or "")))
            if sum(c.isdigit() for c in t) >= 3]


def digits(text):
    """Digit-only forms, for comparing across damaged punctuation."""
    out = set()
    for m in GNUM.findall(htmlmod.unescape(text or "")):
        d = re.sub(r"\D", "", m)
        if len(d) >= 3:
            out.add(d)
    return out


def on_page(value, lines):
    p = _pat_cache.get(value)
    if p is None:
        body = "".join(re.escape(c) if c.isdigit() else SEP for c in value)
        p = _pat_cache[value] = re.compile(r"(?<!\d)" + body + r"(?!\d)")
    return any(p.search(ln) for ln in lines)


def rows_of_html(h):
    rows = []
    for tr in re.split(r"<tr[^>]*>", h or "")[1:]:
        cs = [re.sub(r"\s+", " ", re.sub(r"<[^>]+>", "", c)).strip()
              for c in re.findall(r"<t[dh][^>]*>(.*?)</t[dh]>", tr, re.S)]
        if cs:
            rows.append(cs)
    return rows


def spans(h):
    """Cells declaring a span: the reading kept the header hierarchy."""
    return len(SPAN.findall(h or ""))


def crammed(rows):
    """Columns of single values holding a cell of three or more."""
    if not rows:
        return 0
    n = 0
    for c in range(max(len(r) for r in rows)):
        col = [r[c] for r in rows if c < len(r) and r[c].strip()]
        if len(col) < 3:
            continue
        def val(s):
            return any(ch.isdigit() for ch in s) and not any(ch.isalpha() for ch in s)
        if sum(1 for x in col if val(x)) < len(col) * 0.6:
            continue
        def cnt(s):
            return sum(1 for t in s.split() if any(ch.isdigit() for ch in t))
        if any(cnt(x) >= 3 for x in col) and any(cnt(x) == 1 for x in col):
            n += 1
    return n


def score(rows, page_lines, region_lines):
    """Judge one reading against the page it claims to have read."""
    vals = [t for row in rows for cell in row for t in values(cell)]
    fab = [t for t in vals if not on_page(t, page_lines)]
    emitted = set().union(*[digits(" ".join(r)) for r in rows]) if rows else set()
    truth = set().union(*[digits(l) for l in region_lines]) if region_lines else set()
    return {
        "rows": len(rows), "crammed": crammed(rows),
        "values": len(vals), "fabricated": len(fab),
        "hit": len(emitted & truth), "truth": len(truth),
        "fab_rate": len(fab) / max(1, len(vals)),
        "recall": len(emitted & truth) / max(1, len(truth)),
    }


# --- the runtime decision ---------------------------------------------------
# Thresholds are set from the measured spread, not chosen for roundness.
# Fabrication across seven engines was 0.0-0.1% (gemini-3-flash, ours),
# 1.1-2.5% (sonnet, the 2.5-flash pair), then 14.5-16.7% (nova, haiku).
# Anything above 3% is a different kind of engine, not a worse one.
MAX_FABRICATION = 0.03
MIN_RECALL_RATIO = 0.98   # of ours: a candidate may not lose values to win rows


def accept(candidate, ours):
    """Should an escalated reading replace ours? Returns (bool, reason).

    An escalated reading has to earn its place. It is taken only when it
    fixes the defect that triggered escalation and gives up nothing else --
    which is the same rule the docling tier needed, for the same reason:
    docling was worse than the deterministic pass on 12 of 42 flagged pages
    and returned no table at all on 6.
    """
    if not candidate or not candidate["rows"]:
        return False, "no table"
    if candidate["fab_rate"] > MAX_FABRICATION:
        return False, f"fabricated {candidate['fab_rate']:.1%}"
    if candidate["recall"] < ours["recall"] * MIN_RECALL_RATIO:
        return False, f"recall {candidate['recall']:.1%} < ours {ours['recall']:.1%}"
    if candidate["crammed"] >= ours["crammed"] and ours["crammed"] > 0:
        return False, "did not fix the merged rows"
    return True, "accepted"
