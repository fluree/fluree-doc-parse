"""fluree-doc-parse engine adapter: shells out to `fdoc convert --format md`.

Links are written as their words alone (`--md-links text`), as the
benchmark's ground truth writes them. Set FLUREE_DOC_MD_LINKS=inline to score
the default `[words](address)` form instead.

The notes the converter leaves in HTML comments (`<!-- fluree-doc-parse: ...
-->`, such as content no reader transcribed) are taken out before scoring:
they say what the conversion could not do and are no part of the document's
text, and a rendered Markdown view does not show them. Set
FLUREE_DOC_KEEP_NOTES=1 to score them as written.
"""
import os, re, subprocess

BINARY = os.environ.get("FLUREE_DOC_BINARY", "fdoc")
MD_LINKS = os.environ.get("FLUREE_DOC_MD_LINKS", "text")
KEEP_NOTES = os.environ.get("FLUREE_DOC_KEEP_NOTES") == "1"
TIMEOUT_SECONDS = 120
# A note on a line of its own, with the line break that ends it.
NOTE = re.compile(r"[ \t]*<!-- fluree-doc-parse:.*?-->[ \t]*\n?", re.S)


def without_notes(md):
    return re.sub(r"\n{3,}", "\n\n", NOTE.sub("", md))


def to_markdown(doc_paths, _, output_dir):
    for doc_path in doc_paths:
        base = os.path.splitext(os.path.basename(doc_path))[0]
        cmd = [BINARY, "convert", str(doc_path), "--format", "md",
               "--md-links", MD_LINKS, "--no-escalate", "--quiet"]
        try:
            r = subprocess.run(cmd, capture_output=True, timeout=TIMEOUT_SECONDS, check=False)
            md = r.stdout.decode("utf-8", errors="replace")
            if not KEEP_NOTES:
                md = without_notes(md)
        except subprocess.TimeoutExpired:
            md = ""
        with open(os.path.join(output_dir, f"{base}.md"), "w", encoding="utf-8") as f:
            f.write(md)
