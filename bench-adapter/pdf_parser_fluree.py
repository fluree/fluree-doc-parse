"""fluree-doc-parse engine adapter: shells out to `fdoc convert --format md`.

Links are written as their words alone (`--md-links text`), as the
benchmark's ground truth writes them. Set FLUREE_DOC_MD_LINKS=inline to score
the default `[words](address)` form instead.
"""
import os, subprocess

BINARY = os.environ.get("FLUREE_DOC_BINARY", "fdoc")
MD_LINKS = os.environ.get("FLUREE_DOC_MD_LINKS", "text")
TIMEOUT_SECONDS = 120


def to_markdown(doc_paths, _, output_dir):
    for doc_path in doc_paths:
        base = os.path.splitext(os.path.basename(doc_path))[0]
        cmd = [BINARY, "convert", str(doc_path), "--format", "md",
               "--md-links", MD_LINKS, "--no-escalate", "--quiet"]
        try:
            r = subprocess.run(cmd, capture_output=True, timeout=TIMEOUT_SECONDS, check=False)
            md = r.stdout.decode("utf-8", errors="replace")
        except subprocess.TimeoutExpired:
            md = ""
        with open(os.path.join(output_dir, f"{base}.md"), "w", encoding="utf-8") as f:
            f.write(md)
