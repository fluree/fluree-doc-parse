"""pdf-inspector engine adapter.

Firecrawl's published benchmark used an adapter that is not in either public
repo (their harness shells out to `src/pdf_parser.py --engine pdf-inspector`
and reads $PDF_INSPECTOR_BINARY). This reimplements it against the documented
CLI: `pdf2md <file> --raw` writes markdown to stdout.
"""

import os
import subprocess

BINARY = os.environ.get("PDF_INSPECTOR_BINARY", "pdf2md")

# Per-document ceiling so one pathological PDF cannot stall the corpus run.
TIMEOUT_SECONDS = 120


def to_markdown(doc_paths, _, output_dir):
    for doc_path in doc_paths:
        base_name = os.path.splitext(os.path.basename(doc_path))[0]
        output_file = os.path.join(output_dir, f"{base_name}.md")
        try:
            result = subprocess.run(
                [BINARY, str(doc_path), "--raw"],
                capture_output=True,
                timeout=TIMEOUT_SECONDS,
                check=False,
            )
            markdown = result.stdout.decode("utf-8", errors="replace")
        except subprocess.TimeoutExpired:
            markdown = ""
        # Always write a file, even on failure: the evaluator counts a missing
        # file as a missing prediction, which is the honest outcome for a
        # crash, but an empty string is what a zero-extraction run means.
        with open(output_file, "w", encoding="utf-8") as f:
            f.write(markdown)
