# Anchors

An alternative to [splicing readings in](escalation-tiers.md): have `fdoc`
mark *where* escalated content belongs and let something downstream fill it.

```bash
fdoc convert report.pdf --emit-anchors
```

Every routed region becomes a `doco:Figure` element carrying a token instead
of text:

```
[[VLM:p33:r0]]
```

## The token

```
[[VLM:p{page}:r{region}]]
```

`page` is 0-based; `region` indexes the routed regions on that page, in the
same order [`fdoc triage`](../cli/triage.md) reports them and
`dev render-routed` names its crops (`{stem}_p33_r0.png`). So an anchor, a
crop file and a triage line all name the same thing.

## Why bother

Splicing requires the readings to exist **before** conversion. Anchors invert
that: convert immediately, deliver the deterministic text now, and fill the
escalated regions when the GPU results arrive — which for an async, queue-fed
model service is the difference between a 7 ms response and an 11-second one.

The anchor element is a real element. It carries its `bbox` and `page`, so it
occupies the correct position in reading order and a consumer knows exactly
where the missing content sits on the page.

```jsonc
{ "id": "elem-00214", "type": "doco:Figure", "page": 33,
  "bbox": { "x0": 75.6, "y0": 255.6, "x1": 536.4, "y1": 489.6 },
  "text": "[[VLM:p33:r0]]",
  "provenance": "rust", "evidence": "route" }
```

## Filling them

Replace the token with the reading for the matching crop. The token is stable
and unique within a document, so a plain string substitution is sufficient —
no position tracking, no re-parse.

```
[[VLM:p33:r0]]  ←→  ./readings/report_p33_r0.json
```

## Interaction with --tier-results

`--tier-results` implies `--emit-anchors`: anchors are minted and then filled
in the same run. Passing `--emit-anchors` alone gives you the anchors
unfilled.

Without either flag the deterministic output is **unchanged** — no anchors
appear. Escalation never alters output you did not ask for.

## Choosing between the two

| | anchors | splicing |
|---|---|---|
| output available | immediately | after the models run |
| model service | async, out-of-band | synchronous with conversion |
| arbitration | your responsibility | [applied by `fdoc`](../concepts/escalation.md) |

Splicing is the one to prefer where you can afford to wait, because the
[shape comparison and three-way veto](../concepts/escalation.md) run inside
`fdoc` — a raw substitution gives the model's reading unconditionally, which
is exactly what the arbitration exists to prevent.
