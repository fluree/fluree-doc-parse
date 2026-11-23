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
fdoc dev render-routed report.pdf crops/ --ocr-regions --stored-resolution
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
how many were left. `render-routed --ocr-regions` adds the crops `convert
--ocr` reads, from the same selection and under the same cap; the model's own
crops (pages, routed regions) are written whatever the cap, so past it the
three-step path also reads those.

Given a crops directory, `fdoc ocr` reads its `manifest.jsonl` and only the crops
whose words a page takes, by their `reason`: `scanned`, `near_blank`,
`broken_text`, `region` and `small_region`. A page asked for on a doubt about its
structure already has its text, and a table crop asks about structure; both are
left for a model. Given image files (PNG, JPEG, TIFF), it reads them all,
decoded upright by their EXIF orientation; of a multi-page TIFF, the first page.

The detector reads an image at most 2,000 px on its long side, and scales one
under 736 px wide up to that first. An image so tall that strips of it are
read at least 1.75 times larger than the whole is read in strips that overlap
by a fifth, each line kept once, rather than shrunk: a 1,200 × 9,000 capture
of 220 rows 13 px high read none of them shrunk, and all of them in strips.
A strip of an image 2,000 px wide or more is as long as the image is wide, so
the detector still shrinks it, less than the whole.

A scanned page is never cut: Legal (8.5 × 14 in), the longest, is 1.65 times
as tall as wide. Read in strips, a page took two detections for no line more,
and lost a line running down it longer than the strips' overlap; read whole,
a page at 300 dpi or more is shrunk, and its text still read. A tall capture
is cut, so a sideways line in one longer than the overlap (400 px for strips
of 2,000) is lost or cut short. A wide image is still shrunk, since a cut
across it would cut its lines.

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

### Updates

`fdoc` loads exactly ONNX Runtime 1.28.0 and these PP-OCRv6 small files, by
the hashes compiled into it, and does not redistribute them. So a security fix
in ONNX Runtime reaches users through a new `fdoc` release, not through a new
library dropped into the assets directory (which this build would refuse): the
release updates the pins in `crates/fluree-doc-vision/src/assets.rs` and the
versions and hashes in `tools/ocr-assets/build.py` together (a unit test checks
they agree, and CI builds the pack and reads with it), and a deployment then
rebuilds its assets directory with that release's `build.py`.

### Building

The engine is built into the `fdoc` binary by default (the `ocr` feature of
`fluree-doc-cli`); it runs only when asked for, with `--ocr` and an assets
directory. The libraries (`fluree-doc-pdf`, `fluree-doc-vision`, …) build no
engine unless their own feature asks for it (`fluree-doc-vision/oar`), so a
crate that only parses pulls in no OCR dependency.

ONNX Runtime is loaded into the process at run time rather than linked, so
`fdoc` carries the engine's code and no native library: 5.8 MB more in a
release build (21.8 MB against 16.0 MB without the engine). Building the engine
needs a C compiler: oar-ocr depends on a tokenizer library that compiles C
(Oniguruma), though none of it ends up in `fdoc`. `cargo build
--no-default-features` leaves the engine out, and needs no C compiler.

The engine runs where ONNX Runtime 1.28.0 is published: Linux x64 and arm64
(glibc), macOS on Apple silicon, Windows x64 and arm64. There is none for
Intel macOS (`osx-x64`, which the package no longer ships), and a fully static
build (musl) cannot load a library at all: there, `--ocr oar-ocr` stops with
an error (no ONNX Runtime for the platform, or the library failing to load),
and `--ocr-words` still reads words an outside engine wrote.

## Exit codes

| code | meaning |
|---|---|
| 0 | every image read |
| 1 | at least one image could not be read (each is named on stderr), or `--out` could not be created |
| 2 | bad arguments (two images that would write one word file included), or the assets did not check out; nothing is written |
