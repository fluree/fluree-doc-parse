"""fluree-doc-parse engine adapter: shells out to `fdoc md`."""
import os, subprocess

BINARY = os.environ.get("FLUREE_DOC_BINARY", "fdoc")
TIMEOUT_SECONDS = 120


def to_markdown(doc_paths, _, output_dir):
    for doc_path in doc_paths:
        base = os.path.splitext(os.path.basename(doc_path))[0]
        try:
            r = subprocess.run([BINARY, "md", str(doc_path)],
                               capture_output=True, timeout=TIMEOUT_SECONDS, check=False)
            md = r.stdout.decode("utf-8", errors="replace")
        except subprocess.TimeoutExpired:
            md = ""
        with open(os.path.join(output_dir, f"{base}.md"), "w", encoding="utf-8") as f:
            f.write(md)
