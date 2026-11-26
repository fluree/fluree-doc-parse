# Benchmark adapters

Engine adapters for opendataloader-bench. `pdf_parser_fluree.py` runs
`fdoc convert <pdf> --format md --md-links text --no-escalate`: the
benchmark's reference writes a link as its words alone, and so does
`--md-links text`. Set `FLUREE_DOC_MD_LINKS=inline` to score the default
Markdown instead. [The benchmarks page](../docs/benchmarks/README.md#reproducing-it)
has the full steps.

The model-tier splicing that used to live in `pdf_parser_fluree_hybrid.py` is
now implemented in `fluree-doc-pdf::arbiter`; the hybrid engine is exercised
by pointing the `fdoc` binary at reading caches:

    FDOC_TIER_RESULTS=<vlm-cache> \
    FDOC_OCR_MODELS=<ocr-assets> \
    FDOC_STRUCTURE_RESULTS=<structure-cache> \
    fdoc convert document.pdf --format md --md-links text --no-escalate

The Python adapter is kept for provenance of the published numbers.
