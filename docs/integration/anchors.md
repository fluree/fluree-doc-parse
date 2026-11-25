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
order `dev render-routed` names its crops (`{stem}_p33_r0.png`). So an
anchor and a crop file name the same thing, and the `CROPS` count of
[`fdoc triage`](../cli/triage.md) counts them. Its `ROUTE` lines do not:
they route each page as extracted, and can see one region fewer or more
than the anchors (below).

`triage`, `dev render-routed` and `convert --escalate` all take their
region crops from the anchors of an analysis made with the same options,
rather than routing each page again as extracted: the analysis routes a
page after cleaning it (doubled glyphs dropped, layout rules stripped), and
a label printed in faux bold over a raster region can change which images
the router counts as regions. A region crop is cut where its anchor's
`bbox` sits, in the display frame every box of the output is in, the frame
of a page render. That holds on a page turned to be read upright
(`/Rotate 90`, or drawn sideways), whose regions the router finds in the
turned frame: the box is turned back before the crop is cut.

What is still taken from the page as extracted is the choice of whole
pages: a page the router sends whole there (a scan, a near-blank page,
broken Unicode) is read whole. A page the router sends whole only once
cleaned carries no region anchor, as the analysis mints none on a page it
routes whole, and how it is read depends on how it routes as extracted.
If it routes nothing there, it keeps its deterministic reading. If it
routes regions there (a texty image over half the page, beside a
faux-bold label that then counts half its glyphs), it is read whole, for
the reason `region`: no region crop of it could land, and nothing else
would read it.

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

`--tier-results` mints anchors, and so does escalating with a configured
reader (`--escalate`, or a provider set up in the config): anchors are minted
and then filled in the same run, and an anchor no reading answers is dropped
rather than printed. Neither keeps the unanswered ones the way
`--emit-anchors` does, below. Passing `--emit-anchors` alone gives you the
anchors unfilled.

Passing `--emit-anchors` along with a reader keeps the anchors it does not
answer: what was read in the run lands, and an anchor no reading answers (a
crop `--pages` or `escalation.max_crops` left out, one with no sidecar file)
stays in the output for you to fill. An anchor that was answered drops
either way, whether its reading lost [arbitration](../concepts/escalation.md)
or the reader found nothing printed on the crop: the deterministic text
standing there was chosen, not left waiting. So does a table anchor
`--structure-results` [vetoes](../concepts/escalation.md#the-three-way-veto):
the veto keeps the grid whatever a reading says, so its crop is not sent to
a reader called in the run, and no reading could have replaced it. So do the anchors on a page
that was read whole, whatever that reading made of the page: their regions
were read with it. With a provider configured, add `--no-escalate` for
anchors with no reading in the run at all.

A page read whole is read in place of its regions and tables, so no crop is
named after the anchors on it: `dev render-routed` writes that page as
`{stem}_p33_full.png`. Where such a page went unread (`escalation.max_crops`
left it out, or it has no sidecar file), its anchors stay, and what fills
them is that page's reading, which replaces the page, anchors and all.

Without any of the three (`--emit-anchors`, `--tier-results`, a reader
called in the run) the deterministic output is **unchanged** — no anchors
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
