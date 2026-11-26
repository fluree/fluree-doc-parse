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
left for a model. Given image files (PNG, JPEG, TIFF, …), it reads them all.

## How an image is sized

An image is read at its own resolution, in passes of at most
[`ocr.max_megapixels`](config.md#ocr) (default 12.5) each, decided on its size
alone:

- **Whole**, when it fits: a Letter, A4 or Legal page scanned at 300 dpi (8.4
  to 10.7 Mpx) and a 12 Mpx phone photo (3,024 × 4,032) are read in one pass.
- **In strips** across its full width, each at least as tall as the image is
  wide and within the cap, overlapping by a fifth so a line cut by one strip's
  edge is whole in the next; each line is kept once. A tall capture or a long
  scroll is read this way. A line running down such an image longer than the
  overlap is whole in no strip, and is lost or kept cut short.
- **Shrunk**, with a warning on stderr that names the file, when even a strip as tall as the image
  is wide would not fit: a page scanned at 600 dpi, a wide panorama. Raise
  `ocr.max_megapixels` to read it whole.

The detector misses thin lines in a small image: one whose short side is under
736 px is read larger, toward 736, its long side not past 2,000 px, as the
detector has always enlarged a small image. The engine enlarges it inside its
detector, so the recognizer still reads each line from the image's own pixels:
a crop of 8 to 10 px text 1,200 × 40 read 17 lines this way and 10 at its own
size, and small screenshots (285 × 152, 411 × 90) lost an account number
enlarged only twice. Text under about 6 px is not read at any size. The
enlargement is decided per pass: read whole, an image's long side stays within
2,000 px; read in strips, each is enlarged toward 736 across the image's
width, its length set to fit the cap, which bounds what it costs. So a receipt
400 px wide is read at its own size up to 16,000 px long, and 1.84 times larger
in strips past that.

Shrinking an image to a fixed size loses its small text without a word: a
capture 1,200 × 9,000 shrunk to 2,000 px read none of its 220 rows of 13 px
text, and a 2,400 × 9,000 one none of 1,600; read at their own size, all 220,
and 1,560 of the 1,600 in strips. So the cap is on the pixels a pass holds, which is what a
reading costs, and not on a side.

### Memory and speed

A pass costs about 230 bytes per pixel the detector reads while it is read
(150 to 240 measured): a 12 Mpx photo takes about 2.9 GB at its peak, a page
scanned at 300 dpi about 1.7 GB, with the pipeline's own 0.3 to 0.5 GB. The
engine gives that memory back once the image is read: one memory arena serves
all its pipelines and is shrunk after each pass, so a process that has read
large images holds under 1 GB between them, not the largest image's peak.

The passes read at once share one budget, [`ocr.max_memory`](config.md#ocr),
8 GiB by default (four pages at 300 dpi, or three 12 Mpx photos): a pass that
does not fit beside those being read waits, in turn; one larger than the whole
budget is read alone. So `-j` and the cap set how fast a batch reads, and the
budget bounds what it holds. Measured with `-j4`, four images at once:

| images | 8 GiB budget | no bound |
|---|---|---|
| four 12 Mpx photos | 7.9 GB, 6.6 s | 8.9 GB, 5.5 s |
| four pages at 300 dpi | 6.9 GB, 18 s | 6.9 GB, 19 s |
| four 2,400 × 9,000 captures, in strips | 6.6 GB, 175 s | 10.2 GB, 121 s |
| 60 small crops | 1.3 GB, 18 s | 1.5 GB, 18 s |

A pass waits for a whole reading to end, recognition included, as the engine
reads both in one call: a budget that holds three passes of four reads the
fourth after them, about a third more time.

A lower cap does not save much: a strip costs what a whole image of its size
does, and at 8 Mpx pages and photos are cut in two, read more slowly, and on a
corpus of scanned pages read less text.

## Options

| Option | Description |
|--------|-------------|
| `--models <DIR>` | the OCR assets directory: the models and ONNX Runtime, each checked against the hash `fdoc` was built with. Env: `FDOC_OCR_MODELS`; config: `ocr.models` |
| `--out <DIR>` | where the word files go |
| `--engine <ENGINE>` | `oar-ocr` (the default and only one) |
| `--max-megapixels <MPX>` | most megapixels the engine reads in one pass (default 12.5, from 1 to 128); a larger image is read in strips (see [How an image is sized](#how-an-image-is-sized)). Env: `FDOC_OCR_MAX_MEGAPIXELS`; config: `ocr.max_megapixels` |
| `-j`, `--jobs <N>` | images read at once, each on one thread (`0` = one per core). The engine reads with up to four pipelines at once (fewer on fewer cores), built as needed, about 0.3 GB each between images; a thread finding them all busy waits, and the passes they read share one memory budget (see [Memory and speed](#memory-and-speed)). Measured on twelve cores: 27 s for ten pages with `-j1`, 11 s with `-j4`, no faster with `-j8` |

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
