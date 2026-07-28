# Integration

Everything about putting `fdoc` inside something larger.

- [`fdoc config`](../cli/config.md) — configure a reader once, and escalation
  happens inside `fdoc convert`
- [Wiring the escalation tiers](escalation-tiers.md) — running the tiers
  yourself instead, through files
- [Sidecar formats](sidecar-formats.md) — the exact file contract the tiers
  read
- [Anchors](anchors.md) — the `[[VLM:…]]` protocol for filling escalated
  regions out-of-band
- [Loading into a Fluree ledger](ledger-ingest.md) — DoCO JSON-LD in, graph
  out, re-extraction without a diff
- [Entity overlay](entity-overlay.md) — char offsets → rectangles → highlights
  on a rendered page

## The shape of it

There are two shapes, and the second is the reason the first exists.

**Configured.** `fdoc config gemini --credentials <key>` once, and
`fdoc convert` renders the crops a document asks for, reads them and splices
the results — one command, any output format.

**Through files.** With no provider configured `fdoc` makes no network call
at all: no model runtime, no GPU dependency, and it runs in a Lambda where a
model container cannot. The tiers then read model output from directories of
JSON, so any reader you can run can feed them — your own GPU service, a
provider this build has no client for, a batch that ran last week.

The file path is the one to reach for when the reader is not Google's, when
the readings have to be auditable, or when nothing may leave the machine.

```
          ┌── fdoc triage ──► what needs escalation
          │
  PDF ────┤
          │                     ┌── your GPU service ──┐
          └── fdoc dev          │  layout detector     │
              render-routed ───►│  table structure     │──► sidecar JSON
                  (crops)       │  VLM                 │        │
                                └──────────────────────┘        │
                                                                ▼
              fdoc convert --tier-results <dir> ◄────────────────┘
                    │
                    └──► arbitrated output
```

You own the middle box. `fdoc` tells you what to send it and consumes what
comes back, and the arbitration rules — including [the three-way
veto](../concepts/escalation.md#the-three-way-veto) — are applied on the way
back in.
