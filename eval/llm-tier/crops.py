"""Render one PNG per flagged table, through the pipeline's own crop path.

`fdoc dev render-crops` calls the same `escalate::render_crops` the
pipeline uses, so the evaluation scores the pixels the pipeline actually
sends -- same renderer, same 2x scale, same margin. An earlier version of
this script re-implemented the cropping in Pillow over `fdoc dev
render-pages` output and matched pages by filename; when the renderer's
naming changed, the match silently served the wrong pages and three model
comparisons came back at 1% recall. The contract is now one command wide,
and a manifest entry that yields no crop fails the run instead of
disappearing.

The crop is still the unit, not the page: the strongest crop-unit reader we
measured scored 0.925 TEDS on crops and lost 20-0 to docling on whole
pages, so what a model is shown matters more than which model it is.

    python3 crops.py
"""
import os, subprocess, sys

from common import CORPUS, CROPS, FDOC, REGIONS

if __name__ == "__main__":
    if not os.path.exists(REGIONS):
        sys.exit(f"no {REGIONS}; run regions.py first")
    r = subprocess.run([FDOC, "dev", "render-crops", REGIONS, CORPUS, CROPS])
    sys.exit(r.returncode)
