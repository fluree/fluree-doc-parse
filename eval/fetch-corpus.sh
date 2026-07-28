#!/usr/bin/env bash
# Fetch the eval/corpus PDFs that the published tree cannot carry: three TI
# datasheets and an arXiv paper, whose terms do not permit redistribution.
# cn_gov.pdf and jp_stat.pdf are official government documents — excluded
# from copyright protection — and ship in the tree.
#
# The sha256 recorded beside each URL is the copy that
# eval/expectations/corpus.json was measured against. A mismatch does not
# fail the fetch, because two of the sources are moving targets — TI's
# symlink URLs always serve the current datasheet revision, and cn_arxiv was
# measured on a size-reduced derivative of the arXiv original — but it does
# warn: on a mismatched file the probe's measured aggregates (pages,
# unicode rate, faux-bold count) may drift from the expectations file, and
# that drift is upstream revision, not an engine regression.
set -uo pipefail
cd "$(dirname "$0")/corpus"

status=0
fetch() { # filename url sha256-as-measured
  local name="$1" url="$2" want="$3"
  if [ ! -f "$name" ]; then
    echo "fetching $name from $url"
    if ! curl -fsSL --retry 3 -o "$name" "$url"; then
      echo "FAIL  $name: download failed" >&2
      rm -f "$name"
      status=1
      return
    fi
  fi
  local got
  got=$(shasum -a 256 "$name" | cut -d' ' -f1)
  if [ "$got" = "$want" ]; then
    echo "ok    $name (matches the measured copy)"
  else
    echo "note  $name: upstream differs from the measured copy — probe"
    echo "      aggregates in eval/expectations/corpus.json may drift"
  fi
}

fetch ti_lm358.pdf   "https://www.ti.com/lit/ds/symlink/lm358.pdf"   f78315aaf2d453b0c3cf7c56d968b90e65d8bc93d52c13f8fac2361bfc6ae1ee
fetch ti_ne555.pdf   "https://www.ti.com/lit/ds/symlink/ne555.pdf"   9800ba0d037333a442e18704e5df765a7a202c4b661f4135d6385eea7c073c1c
fetch ti_tps5430.pdf "https://www.ti.com/lit/ds/symlink/tps5430.pdf" f0e6a520fb14ae4ef073d8faf0cb5c6bceec29371eb63759e5d5593443282e13
fetch cn_arxiv.pdf   "https://arxiv.org/pdf/2412.07626"              2160acf355867ecdcec5e2d0253f8dd55979b158d9b2ca07089442500e49e562

exit $status
