# fdoc ocr

Read crops or images with the built-in OCR engine, into word files.

```bash
fdoc ocr <DIR|IMAGE>... --models <DIR> --out <DIR> [options]
```

Writes one [OCR words](../integration/sidecar-formats.md#4-ocr-words----ocr-words)
file per image, `<image stem>.json`, which `fdoc convert --ocr-words` reads. It
is the middle step of the three-step path, for when the crops, the reading and
the conversion run in different places or at different times:

```bash
fdoc dev render-routed report.pdf crops/ --ocr-regions
fdoc ocr crops/ --models ~/assets/ocr --out words/
fdoc convert report.pdf --ocr-words words/
```

`fdoc convert --ocr oar-ocr --models DIR` does the same in one command, in
memory, and gives the same output. On a page asked for whole on a doubt
about its structure, `render-routed --ocr-regions` gives the page its doubt's
reason and crops its raster regions apart, so `fdoc ocr` reads the regions,
as `convert --ocr` does, and not the page.

An image is read only if its own pixels can hold a line of text: 10 px on its
short side, whatever size it is drawn at (measured: the engine reads a line
exactly from 10 px tall, and misreads it below). A form's checkboxes drawn
from 8 × 8 masks and rules stretched from a 1 × 1 mask are not read, and an
image drawn twice in one place is read once. A document's crops are capped
by [`ocr.max_crops`](config.md#ocr) (default 1000): past it, pages read whole
come first, then routed regions, then small ones, and a note on stderr says
how many were left.

Given a crops directory, `fdoc ocr` reads its `manifest.jsonl` and only the crops
whose words a page takes, by their `reason`: `scanned`, `near_blank`,
`broken_text`, `region` and `small_region`. A page asked for on a doubt about its
structure already has its text, and a table crop asks about structure; both are
left for a model. Given image files (PNG, JPEG, TIFF, …), it reads them all.

## Options

| Option | Description |
|--------|-------------|
| `--models <DIR>` | the OCR assets directory: the models and ONNX Runtime, each checked against the hash `fdoc` was built with. Env: `FDOC_OCR_MODELS`; config: `ocr.models` |
| `--out <DIR>` | where the word files go |
| `--engine <ENGINE>` | `oar-ocr` (the default and only one) |
| `-j`, `--jobs <N>` | images read at once, each on one thread (`0` = one per core). The engine reads with up to four pipelines at once (fewer on fewer cores), built as needed, each 0.6 to 1 GB once it has read pages (more for larger ones); a thread finding them all busy waits. Measured on twelve cores: 27 s for ten pages with `-j1`, 11 s with `-j4`, no faster with `-j8` |

## The assets directory

```text
SHA256SUMS                          every file below, for a person to check by
pp-ocrv6-small/det.onnx             text detection     (PaddlePaddle, Apache-2.0)
pp-ocrv6-small/rec.onnx             text recognition
pp-ocrv6-small/dict.txt             the recognizer's characters
onnxruntime/<platform>/<library>    ONNX Runtime 1.28.0 (Microsoft, MIT)
NOTICE, pp-ocrv6-small/LICENSE, onnxruntime/LICENSE, onnxruntime/ThirdPartyNotices.txt
sbom.cdx.json                       CycloneDX 1.5: the runtime and the models, for an SBOM
```

`tools/ocr-assets/build.py` builds it from the publishers' own files at pinned
versions: PaddlePaddle's ONNX exports of PP-OCRv6 small and Microsoft's ONNX
Runtime package. It holds the ONNX Runtime library of every platform shipped
(`linux-x64`, `linux-arm64`, `osx-arm64`, `win-x64`, `win-arm64`), or of those
given with `--platforms`; `fdoc` loads the running platform's. Each file `fdoc`
loads must hash to the value this build of `fdoc` pins: `SHA256SUMS` sits
beside the files, so whoever can replace a file can rewrite it, and is not what
is checked. A file that is missing or changed stops the run with its name; a
pack of other versions needs a build of `fdoc` that pins them (`build.py`
prints the table).

What is checked is what is used. The models are read once and handed to the
engine as the bytes that were hashed. On Linux the library is loaded through
the file that was hashed, still open, so a file put in its place afterwards is
not the one loaded; elsewhere it is loaded again by its path, so keep the
directory no more writable than the `fdoc` binary. A relative `--models` is
taken from the working directory. If ONNX Runtime fails to load, nothing is
loaded in its place: the library given is the only one tried.

ONNX Runtime is loaded into the process at run time rather than linked, so
`fdoc` carries the engine's code (about 6 MB, 5 MB stripped) and no native
library. A build that is fully static (musl) cannot load it and has no built-in
engine; it still reads `--ocr-words`. Building the engine needs a C compiler:
oar-ocr depends on a tokenizer library that compiles C (Oniguruma), though
none of it ends up in `fdoc`. `cargo build --no-default-features` leaves the
engine out.

## Exit codes

| code | meaning |
|---|---|
| 0 | every image read |
| 1 | at least one image could not be read (each is named on stderr), or `--out` could not be created |
| 2 | bad arguments (two images that would write one word file included), or the assets did not check out; nothing is written |
