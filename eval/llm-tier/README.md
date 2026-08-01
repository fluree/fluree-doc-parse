# LLM escalation tier — model selection and the acceptance rubric

Measures whether a multimodal model should sit above the deterministic
reader, which model, and — the part that outlives any model — how the
pipeline decides at runtime that a reading has earned its place.

Measured 2026-07-27 on 115 tables from 42 flagged pages across 9 documents.

## The population

Not "all tables": the tables `table::suspect_tables` reports as `Fragmented`
or `MergedRows`, which is the only population an escalation tier ever sees.
Measuring a model on tables we already read correctly would flatter it
against a baseline that never fires. That trigger is `fdoc triage`, and it
fires on 1.8% of benchmark tables and 10.6% of real-world ones.

## The rubric

Every measure is computed from the PDF's own glyph layer, so it needs no
reference table and can therefore run in the pipeline, not just in an
experiment. `rubric.py` is the whole thing.

| measure | question |
|---|---|
| `fabrication` | values emitted that are printed nowhere on the page |
| `recall` | values printed inside the table that the reading captured |
| `crammed` | columns of single values holding a cell of three or more — a collapsed merged row |

Our own reading is scored by the same function and must come out near zero
fabricated, since it can only report glyphs that exist. **That control is
what makes a model's number trustworthy.** Three earlier versions of this
measure produced confident, wrong rates (50.8%, 72.9%, 52.4%) and the
control would have caught every one:

- Using our structured text as the denominator measures our defect, not the
  model's — these are the pages we get *wrong*. Use the glyph layer.
- Joining glyphs without gap detection fuses adjacent column values into
  one token, so real numbers look invented.
- `stress_erp2023` maps its decimal point to nothing, so `200.52` is
  `200<fffd>52` in the glyph layer. Literal matching scores a correct
  reading as fabricated. Matching is punctuation-tolerant for this reason.

## Results

Seven engines, scored on the 105 tables every engine returned:

| engine | recall | fab% | crammed | $/105 | median |
|---|---|---|---|---|---|
| ours (deterministic) | 95.9% | 0.1 | 40 | — | 7ms |
| **gemini-3-flash, thinking LOW** | **98.8%** | **0.0** | 2 | 1.42 | 2.5s |
| gemini-2.5-pro | 97.5% | 1.2 | 2 | 5.39 | 26.9s |
| claude-sonnet¹ | 96.7% | 1.3 | 3 | n/a | — |
| gemini-2.5-flash-lite, thinking 0 | 95.6% | 2.0 | 1 | 0.07 | 2.2s |
| gemini-2.5-flash, thinking 0 | 93.9% | 2.5 | 3 | 0.36 | 3.1s |
| nova-pro | 81.4% | 14.5 | 5 | 0.32 | 1.8s |
| claude-haiku¹ | 78.1% | 16.7 | 1 | n/a | — |

¹ agent-mediated rather than single-shot, so cost and latency are not
comparable; accuracy is.

**Reasoning effort is strictly harmful on transcription.** Confirmed three
ways. Uncapped thinking on gemini-3-flash: 27.8s median instead of 2.5s,
896k thinking tokens instead of 5.8k, 17 failures instead of 3 (thinking
starved the output budget into `MAX_TOKENS`), $8.29 instead of $1.42 — at
identical accuracy. 2.5-flash with thinking cost 14x flash-lite for the same
score. 2.5-pro, which cannot disable thinking, spent 505k thinking tokens to
score *below* flash's 134k. Transcription is perception, not deduction.

**Our defect is structural, not lossy.** On the 8 densest tables we score
99.9% recall and 100% precision, then collapse 407 printed rows into 227. So
the integration takes row and column structure from the model and keeps cell
text from the glyph layer.

## Re-measured 2026-07-30, on the newer models

Same 115 crops, same prompt, thinking LOW. Scored on the 107 tables every run
returned a table for, so these compare to each other rather than to the table
above.

| engine | recall | fab% | out+think tokens | $/1k tables |
|---|---|---|---|---|
| ours (deterministic) | 95.9% | 0.1 | — | — |
| **gemini-3.6-flash** | **98.8%** | **0.1** | 95,437 | 8.73 |
| gemini-3-flash-preview *(the configured default)* | 97.9% | 1.0 | 133,804 | 4.43 |
| **gemini-3.5-flash-lite** | 97.7% | 1.0 | **58,541** | **1.78** |
| gemini-2.5-pro | 97.5% | 1.2 | 504,925 | 50.41 |
| gemini-2.5-flash-lite | 95.6% | 2.0 | 106,777 | 0.66 |

Two results worth acting on. **3.6-flash is the best measured on both axes**,
on 71% of the tokens the default spends. **3.5-flash-lite matches the default
within noise on 44% of its output tokens**, for 2.5x less — and its 2.5
ancestor was rejected here at 2.0% fabrication, which two generations halved.

The default is unchanged. It is a *preview* model priced at $0.50/$3.00 per
million against $1.50/$7.50 for the generally-available tier, with no announced
retirement; whatever replaces that pricing is the reason to move, and the
choice between the two candidates is a quality-versus-cost decision about a
corpus rather than a fact about the models.

`gemini-3-flash-preview` is not a rolling alias for the latest. On identical
crops it reads worse than 3.6-flash and spends 40% more output tokens, which a
pointer to 3.6 would not.

**Reasoning is 57% of the billed output** on the pipeline's own workload, and
the price sheet bills it at the output rate — so the thinking cap is a cost
decision as much as a quality one.

A measurement error worth recording, because it looked like a decisive result:
the first run of this comparison returned 1.4% recall and 76% fabrication for
three unrelated models. The crops were misaligned — the old `crops.py` matched
renders by filename, the renderer's naming had changed, and a fallback sorted
filenames and indexed by page number, so page 28 got the 28th name in
`p0, p1, p10, p11, p2` order. Nothing failed; the numbers just looked like a
model comparison. `crops.py` now calls the pipeline's own crop path, and a
manifest entry yielding no crop fails the run.

## The arbiter is what makes this safe

`rubric.accept()` decides whether an escalated reading replaces ours. With
it applied, **the model choice stops being a correctness question**:

| pipeline | crammed | recall | fab% | taken |
|---|---|---|---|---|
| deterministic only | 44 | 97.0% | 0.1% | — |
| + gemini-3-flash | 14 | 99.7% | 0.0% | 95/115 |
| + gemini-2.5-flash-lite | 16 | 99.5% | 0.1% | 96/115 |
| + claude-sonnet | 18 | 99.6% | 0.0% | 94/115 |
| + nova-pro | 22 | 99.6% | 0.0% | 86/115 |
| + claude-haiku | 29 | 99.6% | 0.0% | 88/115 |

Nova and Haiku fabricate 14.5% and 16.7% raw and still yield a 0.0% pipeline.
A better model does not buy safety — it buys *yield*, measured in tables
whose merged rows get fixed.

**One rung is enough.** A second backend after the first buys 3 tables of
115; a third buys zero (sonnet was taken 0 times behind the other two). The
~13 tables still crammed are a floor no model in this set clears.

## What the crop is decides more than which model reads it

The rubric above scores *table* crops, where the reader is told it is looking
at a table. The 200-document benchmark also sends *region* crops, which the
router produces knowing only that the text layer failed there — chart, scan
or image-table alike. Three defects lived in that gap, all of them in the
harness rather than in the model:

| defect | cost | fix |
|---|---|---|
| crop cut to our grid, not the real table | doc 150: TEDS 0.36 | union with the layout box (already in `render-routed`; the cache predated it) |
| region prompt left the table/prose choice to the model | docs 110, 122: TEDS 0.00 | `manifest.jsonl` carries the detector's `table` flag; the prompt states it |
| a refusal spliced as body text | 5 documents | dropped in `clean()`, and again in `arbiter::not_a_reading` |

Doc 150 is the one to keep in mind. Given a crop holding the right-hand half
of a table, gemini-3-flash transcribed the right-hand half of the table —
correctly. Nothing in a shape or fidelity check can see that, because the
reading is faithful to what it was shown. **A tier's accuracy is bounded by
its crop, and a crop bug reads exactly like a model bug.**

Measured on the 200-document benchmark, all three fixed:

| | overall | NID | TEDS | MHS |
|---|---|---|---|---|
| before | 0.910644 | 0.9396 | 0.9039 | 0.8201 |
| after | **0.916932** | 0.9426 | **0.9489** | 0.8216 |

16 documents better, 10 worse, 174 unchanged. The losses are all chart
regions where a legend moved in reading order, none over 0.035.

Two rules that look like prompt engineering are in code, because a model
complies with them only most of the time: a reading that is prose *about* the
image is dropped, and a one-row `<table>` is unwrapped to lines (three
picture captions side by side are not a table).

## Running it

Needs `eval/corpus-gaps/` (test-only PDFs, gitignored — not redistributable)
and a release `fdoc`. Work products land in `work/`, also gitignored.

```sh
python3 regions.py                       # trigger -> region manifest
python3 crops.py                         # pipeline crop path -> one PNG per table
python3 groundtruth.py                   # glyph-layer indexes

export GOOGLE_APPLICATION_CREDENTIALS=/path/to/sa.json
python3 run_gemini.py gemini-3-flash-preview work/runs/gemini-3-flash work/crops 0
                                         #                    thinking budget ^
python3 run_bedrock.py us.amazon.nova-pro-v1:0 work/runs/nova-pro work/crops

python3 score.py work/runs/*             # rank them
python3 score.py --only stress work/runs/*   # the hard cases, unaveraged
```

Crop resolution is 2x, landing most tables at 1000–1200px on the long edge.
Measured: 1x, 1.5x and 2x all bill the same input tokens (the API resizes
below a threshold); 3x costs 2.5x as many and read *worse*. There is nothing
to buy above this.
