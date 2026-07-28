# Benchmark adapters

Engine adapters for opendataloader-bench. The model-tier splicing that used
to live in `pdf_parser_fluree_hybrid.py` is now implemented in
`fluree-doc-pdf::arbiter`; the hybrid engine is exercised by pointing the
`fdoc` binary at reading caches:

    FDOC_TITLE_BOXES=<layout-cache> \
    FDOC_TIER_RESULTS=<vlm-cache> \
    FDOC_STRUCTURE_RESULTS=<structure-cache> \
    fdoc md document.pdf

The Python adapter is kept for provenance of the published numbers.
