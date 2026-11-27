#!/usr/bin/env bash
# Evaluation harness. See eval/TEST_PLAN.md for what each check means.
set -uo pipefail
cd "$(dirname "$0")/.."

echo "### unit tests"
cargo test --quiet 2>&1 | grep -E "test result|^error" || true

echo
echo "### T0/T1 — corpus probe"
for f in ti_lm358 ti_ne555 ti_tps5430 cn_arxiv; do
  if [ ! -f "eval/corpus/$f.pdf" ]; then
    echo "FAIL — eval/corpus/$f.pdf missing; run ./eval/fetch-corpus.sh first"
    exit 1
  fi
done
cargo build --release --quiet
./target/release/fdoc dev probe eval/corpus
probe_rc=$?

echo
echo "### span resolution (overlay mechanism)"
./target/release/fdoc dev find eval/corpus/jp_stat.pdf "検討会の構成"

echo
echo "### tough cases (eval/tough; fetch the documents with eval/tough/check.py --fetch)"
python3 eval/tough/check.py
tough_rc=$?

echo
echo "### scanned pages are routed (eval/scanned; generate with uv run eval/scanned/make_scans.py)"
scanned_rc=0
scanned_note="scanned pages routed"
if [ -f eval/scanned/corpus/index.json ]; then
  if [ -n "${FDOC_OCR_MODELS:-}" ]; then
    # With the OCR assets, the OCR run too, held to its baseline.
    python3 eval/scanned/check.py --baseline eval/scanned/baseline.json
    scanned_rc=$?
    scanned_note="scanned pages routed and read against the baseline"
  else
    python3 eval/scanned/check.py --no-ocr
    scanned_rc=$?
    scanned_note="scanned pages routed (set FDOC_OCR_MODELS for the OCR run)"
  fi
else
  echo "skipped — eval/scanned/corpus/ not generated"
  scanned_note="scanned set not generated"
fi

echo
if [ $probe_rc -ne 0 ]; then
  echo "FAIL — probe reported a hard-gate failure (see TEST_PLAN.md T0/T1)"
  exit 1
fi
if [ $tough_rc -ne 0 ]; then
  echo "FAIL — a tough case failed or its document is missing (see eval/tough/README.md)"
  exit 1
fi
if [ $scanned_rc -ne 0 ]; then
  echo "FAIL — the scanned-set check failed: a routing gate, a born-digital page's headings, a fact read with no reader, a fact or heading lost against the baseline, or fdoc itself (see its output above and eval/scanned/README.md)"
  exit 1
fi
echo "PASS — T0/T1 gates met, tough cases hold, $scanned_note. T2 (layout) not yet implemented; see TEST_PLAN.md."
