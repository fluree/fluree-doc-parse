# OCR assets

`build.py` builds the directory that `fdoc --ocr oar-ocr --models DIR` loads:
the PP-OCRv6 small models and ONNX Runtime, with a `SHA256SUMS` over every file.
`fdoc` checks each file it loads against the hash it was built with (`PINNED`
in `crates/fluree-doc-vision/src/assets.rs`), not against that list: a list
beside the files vouches for nothing to someone able to replace both. The
build prints the table for `PINNED`; a pack of other files is another `fdoc`
build. The output directory must be new or empty.

```bash
python3 -I tools/ocr-assets/build.py ~/assets/ocr                         # every platform
python3 -I tools/ocr-assets/build.py ~/assets/ocr --platforms linux-x64   # one
```

Nothing is converted or rebuilt. Each file is a publisher's own, at a pinned
version, checked twice: the download against the hash its source states, and the
file written against the hash pinned in `build.py`.

| file | source | licence |
|---|---|---|
| `pp-ocrv6-small/det.onnx` | PaddlePaddle's ONNX export, `PaddlePaddle/PP-OCRv6_small_det_onnx` on Hugging Face, revision `28fe5895` | Apache-2.0 |
| `pp-ocrv6-small/rec.onnx` | `PaddlePaddle/PP-OCRv6_small_rec_onnx`, revision `b8f84f0b` | Apache-2.0 |
| `pp-ocrv6-small/dict.txt` | the `character_dict` of the `inference.yml` published with the recognizer | Apache-2.0 |
| `onnxruntime/<platform>/…` | Microsoft's build, NuGet `Microsoft.ML.OnnxRuntime` 1.28.0 | MIT |

**Licences.** Both licences are permissive and both ask that their terms
travel with the files, so the directory carries them: `pp-ocrv6-small/LICENSE`
(the Apache-2.0 text), `onnxruntime/LICENSE` (MIT) and
`onnxruntime/ThirdPartyNotices.txt` (the notices of what ONNX Runtime itself
includes): the Apache-2.0 text as apache.org publishes it, the other two from
Microsoft's package, with a `NOTICE` naming each file's source. A deployment that ships this directory ships them with it, and should
name ONNX Runtime and the PP-OCRv6 models in its own third-party notices and
SBOM: they are native code and data loaded at run time, which a scan of the
Rust dependencies does not see.

**SBOM.** `sbom.cdx.json` declares them for that SBOM, as a CycloneDX 1.5
fragment to merge into the one a release generates from its Rust
dependencies: ONNX Runtime as one component per platform library shipped,
with its package URL (`pkg:nuget/Microsoft.ML.OnnxRuntime@1.28.0`) and CPE so a
vulnerability scanner matches it to advisories, and the two models and the
character list with their Hugging Face revisions. Each component carries the
SHA-256 of the file shipped and its licence.

ONNX Runtime compiles third-party libraries into its own (abseil, protobuf,
onnx, re2, flatbuffers, Eigen, and more; KleidiAI on arm64, WIL on Windows,
CoreML and Dawn support on macOS), which a scanner matching only ONNX Runtime
would miss, so each library's component declares them as nested components
with their versions, package URLs and licences. The list is written out in
`EMBEDDED` in `build.py`, which says what is included and why. The versions are
the ones the package's build used: Microsoft builds it with vcpkg, so a library
vcpkg provides has the version vcpkg resolves (from ONNX Runtime's
`cmake/vcpkg.json` and the vcpkg baseline it names), and the others the one in
ONNX Runtime's `cmake/deps.txt`. Every build checks each version against those
files at the commit the package was built from (`da9b5e36`, tag `v1.28.0`),
pinned by hash like everything else: ONNX Runtime's from the Software Heritage
archive of its repository (github.com/microsoft/onnxruntime answers 404 since
November 2026), the vcpkg baseline from github.com/microsoft/vcpkg. It also
checks that the libraries carry a trace of some of them (abseil's and
nlohmann/json's versioned namespaces, protobuf's version string, KleidiAI's
symbols).

The fragment is deterministic (no
timestamp, a serial number derived from the files), so the same build gives the
same `SHA256SUMS`.

Downloads are cached under `~/.cache/fdoc-ocr-assets` (`--cache`), so a rebuild
reads nothing from the network. To move to another version, change the pinned
revision or version and its hashes together; the build fails on any file whose
hash is not the pinned one, so a source that changes under the same name is
caught.

The directory is about 140 MB with every platform's ONNX Runtime, 55 MB with one.
