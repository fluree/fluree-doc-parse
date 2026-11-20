#!/usr/bin/env python3
"""Build the OCR assets directory `fdoc --ocr oar-ocr --models DIR` loads.

  python3 -I tools/ocr-assets/build.py OUT_DIR [--cache DIR] [--platforms linux-x64,osx-arm64,...]

Everything comes from its publisher, at a pinned version, and is checked twice:
the download against the hash its source states, and every file written against
the hash pinned here. Nothing is converted:

- the PP-OCRv6 small models are PaddlePaddle's own ONNX exports, at a fixed
  Hugging Face revision (Apache-2.0); the recognizer's character list is taken
  from the `inference.yml` published beside it;
- ONNX Runtime is Microsoft's build, from the `Microsoft.ML.OnnxRuntime` NuGet
  package (MIT), one native library per platform.

The output holds `SHA256SUMS` over every file, for a person or a tool to check
by; `fdoc` checks the files it loads against the hashes it was built with
(`PINNED` in crates/fluree-doc-vision/src/assets.rs), which this prints. The
output directory must be new or empty, so it holds nothing but this build.
Standard library only, so it runs anywhere Python 3.9+ does.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import urllib.request
import zipfile

DET_REV = "28fe5895c24fd108c19eb3e8479f4ab385fbfc62"
REC_REV = "b8f84f0b80c529de40b4fbb3544b84fa7233a513"
HF = "https://huggingface.co/PaddlePaddle/{repo}/resolve/{rev}/{name}"
NUGET = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/{v}/microsoft.ml.onnxruntime.{v}.nupkg"
ORT_VERSION = "1.28.0"
# The commit the package was built from: the nuspec's <repository commit=...>, which
# is also the v1.28.0 tag. github.com/microsoft/onnxruntime has answered 404 since
# November 2026, so its files are read from Software Heritage's archive of that
# repository (last full visit 2026-11-14), by git blob id: the URL names the bytes.
ORT_COMMIT = "da9b5e364c465de65c49d91e696cd6485270757f"
SWH = "https://archive.softwareheritage.org/api/1/content/sha1_git:{id}/raw/"
# The microsoft/vcpkg commit ONNX Runtime's cmake/vcpkg-configuration.json names as
# its baseline (checked at build time).
VCPKG_BASELINE = "120deac3062162151622ca4860575a33844ba10b"

# Downloads: (cache name, url, sha256 the source states).
SOURCES = {
    "det.onnx": (
        HF.format(repo="PP-OCRv6_small_det_onnx", rev=DET_REV, name="inference.onnx"),
        "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    ),
    "rec.onnx": (
        HF.format(repo="PP-OCRv6_small_rec_onnx", rev=REC_REV, name="inference.onnx"),
        "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    ),
    "rec.yml": (
        HF.format(repo="PP-OCRv6_small_rec_onnx", rev=REC_REV, name="inference.yml"),
        "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1",
    ),
    "onnxruntime.nupkg": (
        NUGET.format(v=ORT_VERSION),
        "769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db",
    ),
    # The models' licence, whose text Apache-2.0 asks to travel with them.
    "apache-2.0.txt": (
        "https://www.apache.org/licenses/LICENSE-2.0.txt",
        "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    ),
    # What ONNX Runtime's build resolves its dependencies from, at the commit the
    # package was built from (see ORT_COMMIT): read only to check EMBEDDED.
    "ort-deps.txt": (SWH.format(id="d6a5b71221dc8eff011b6d56205aa0ec591a711a"),  # cmake/deps.txt
                     "894bd3f342b9fa570301f1a62830751e50657ec98a22b210d176b7dbb354ccc2"),
    "ort-vcpkg.json": (SWH.format(id="e9ea52e4b5248e502e5b933d7d8594496ad95ca1"),  # cmake/vcpkg.json
                       "852bff0116ae6a6dc06e418e342fb6872652f682879cfde2df47f47adffa6be9"),
    "ort-vcpkg-configuration.json": (SWH.format(id="ad4be7d57c2209cb77504b4cffede302984f5c5e"),  # cmake/vcpkg-configuration.json
                                     "395474176a70d074c01978dca9f48319bcda1ba1cc52f85425a04550f5475426"),
    "vcpkg-baseline.json": (
        f"https://raw.githubusercontent.com/microsoft/vcpkg/{VCPKG_BASELINE}/versions/baseline.json",
        "19a9764b78d17dc276df04dc9af95efe221b34027f15a03c33839b9d56d0d9eb",
    ),
}

# ONNX Runtime per platform: (path in the package, output path, sha256).
RUNTIMES = {
    "linux-x64": ("runtimes/linux-x64/native/libonnxruntime.so", "onnxruntime/linux-x64/libonnxruntime.so",
                  "1461ef7cc3d9e49982591721683cc3e3a55580aeca9a5254e7aac47b75ee4bab"),
    "linux-arm64": ("runtimes/linux-arm64/native/libonnxruntime.so", "onnxruntime/linux-arm64/libonnxruntime.so",
                    "f1ec1a08eb99bd6e5401340f0a2b101381bf4694415480291dc13bcaa30f9ec7"),
    "osx-arm64": ("runtimes/osx-arm64/native/libonnxruntime.dylib", "onnxruntime/osx-arm64/libonnxruntime.dylib",
                  "244e53902258550e4b3afc3231c3cf8ba860173fc47a1fb572082ed822e6973f"),
    "win-x64": ("runtimes/win-x64/native/onnxruntime.dll", "onnxruntime/win-x64/onnxruntime.dll",
                "18370c375f07357fa5874344a9d9ac17e6b6fe1eb18b1dd209d79483b4470257"),
    "win-arm64": ("runtimes/win-arm64/native/onnxruntime.dll", "onnxruntime/win-arm64/onnxruntime.dll",
                  "102342326311ad62879b808840b4e3fd1372c41d8c871b59536d89a3c64d53f2"),
}

# The models as written: the same bytes as downloaded, and the character list.
MODELS = {
    "pp-ocrv6-small/det.onnx": "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    "pp-ocrv6-small/rec.onnx": "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    "pp-ocrv6-small/dict.txt": "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
}

# Third-party code compiled into each ONNX Runtime library, declared in the SBOM as
# components nested in that library's, so a scanner sees, say, the protobuf inside.
#
# Included: what the library links statically in the build Microsoft packages,
# the onnxruntime_EXTERNAL_LIBRARIES of cmake/onnxruntime_external_deps.cmake
# (abseil, protobuf, onnx, re2, flatbuffers, nlohmann/json, GSL, SafeInt, date,
# Boost.Mp11, Eigen, and WIL on Windows) and cpuinfo, plus what the package's
# options add per platform: KleidiAI in MLAS on arm64, and the CoreML and WebGPU
# execution providers on macOS (coremltools, FP16, psimd; Dawn, Tint included).
# Header-only libraries count: their code is instantiated in the library.
# Each is confirmed in the shipped bytes (build paths, symbols, version strings;
# MARKER below checks some of them on every build).
# Excluded: what deps.txt lists for builds this package is not (tests, benchmarks,
# Python, training, CUDA/TensorRT/DirectML/other execution providers, XNNPACK and
# its pthreadpool, mimalloc, onnxruntime-extensions, the QMX KleidiAI variant),
# and build tools that leave no code in the library (protoc, vcpkg's
# boost-config, utf8-range and zlib, which nothing in the library links).
#
# Versions: the package's libraries are built with vcpkg (their build paths name
# vcpkg_installed), so a dependency vcpkg provides takes the version vcpkg
# resolves, not the one deps.txt states: an `overrides` entry of
# cmake/vcpkg.json, else the higher of the vcpkg baseline and the manifest's
# `version>=`. That is why re2, nlohmann/json, date and Boost.Mp11 differ from
# deps.txt; the library carries nlohmann/json's json_abi_v3_12_0, not 3.11.3.
# Abseil, protobuf, onnx, Eigen and cpuinfo are vcpkg overlay ports in
# cmake/vcpkg-ports whose portfiles pin the same refs as deps.txt, so deps.txt
# states them; KleidiAI, Dawn, coremltools, FP16 and psimd are not in vcpkg.json
# and are fetched from deps.txt.
#
# The rows are written out rather than derived, because deriving them would mean
# re-implementing vcpkg's version resolution and the package's build options.
# Each row names where its version comes from, `("deps", name, ref)` (the tag or
# commit of deps.txt's URL) or `("vcpkg", port, version)`, and the build checks it
# against the pinned sources, so a version bump that changes them fails until the
# rows follow.
EMBEDDED = [
    # (name, version, purl, SPDX licence, platforms or None for all, source, marker)
    ("abseil-cpp", "20250814.0", "pkg:github/abseil/abseil-cpp@20250814.0", "Apache-2.0", None,
     ("deps", "abseil_cpp", "20250814.0"), b"lts_20250814"),
    ("protobuf", "3.21.12", "pkg:github/protocolbuffers/protobuf@v3.21.12", "BSD-3-Clause", None,
     ("deps", "protobuf", "v21.12"), b"3.21.12"),
    ("onnx", "1.22.0", "pkg:github/onnx/onnx@v1.22.0", "Apache-2.0", None,
     ("deps", "onnx", "v1.22.0"), None),
    ("re2", "2025-08-12", "pkg:github/google/re2@2025-08-12", "BSD-3-Clause", None,
     ("vcpkg", "re2", "2025-08-12"), None),
    ("flatbuffers", "23.5.26", "pkg:github/google/flatbuffers@v23.5.26", "Apache-2.0", None,
     ("vcpkg", "flatbuffers", "23.5.26"), None),
    ("nlohmann-json", "3.12.0", "pkg:github/nlohmann/json@v3.12.0", "MIT", None,
     ("vcpkg", "nlohmann-json", "3.12.0"), b"json_abi_v3_12_0"),
    ("GSL", "4.2.1", "pkg:github/microsoft/gsl@v4.2.1", "MIT", None,
     ("vcpkg", "ms-gsl", "4.2.1"), None),
    ("SafeInt", "3.0.28", "pkg:github/dcleblanc/safeint@3.0.28", "MIT", None,
     ("vcpkg", "safeint", "3.0.28"), None),
    ("date", "3.0.4", "pkg:github/howardhinnant/date@v3.0.4", "MIT", None,
     ("vcpkg", "date", "3.0.4"), None),
    ("boost-mp11", "1.88.0", "pkg:github/boostorg/mp11@boost-1.88.0", "BSL-1.0", None,
     ("vcpkg", "boost-mp11", "1.88.0"), None),
    ("eigen", "1d8b82b0740839c0de7f1242a3585e3390ff5f33",
     "pkg:gitlab/libeigen/eigen@1d8b82b0740839c0de7f1242a3585e3390ff5f33", "MPL-2.0", None,
     ("deps", "eigen", "1d8b82b0740839c0de7f1242a3585e3390ff5f33"), None),
    ("cpuinfo", "4628dc060ce4e82345dc166bbac875609db4ff69",
     "pkg:github/pytorch/cpuinfo@4628dc060ce4e82345dc166bbac875609db4ff69", "BSD-2-Clause", None,
     ("deps", "pytorch_cpuinfo", "4628dc060ce4e82345dc166bbac875609db4ff69"), b"cpuinfo_"),
    ("wil", "1.0.250325.1", "pkg:github/microsoft/wil@v1.0.250325.1", "MIT", {"win-x64", "win-arm64"},
     ("vcpkg", "wil", "1.0.250325.1"), b"\\wil\\"),
    ("kleidiai", "1.20.0", "pkg:github/arm-software/kleidiai@v1.20.0", "Apache-2.0",
     {"linux-arm64", "osx-arm64", "win-arm64"}, ("deps", "kleidiai", "v1.20.0"), b"kai_run_"),
    ("dawn", "ec7b457e5bb1fcec6f59733c4f3dd84d2f885a38",
     "pkg:github/google/dawn@ec7b457e5bb1fcec6f59733c4f3dd84d2f885a38", "BSD-3-Clause", {"osx-arm64"},
     ("deps", "dawn", "ec7b457e5bb1fcec6f59733c4f3dd84d2f885a38"), b"_deps/dawn-src/src/dawn"),
    ("coremltools", "7.1", "pkg:github/apple/coremltools@7.1", "BSD-3-Clause", {"osx-arm64"},
     ("deps", "coremltools", "7.1"), b"CoreML.Specification"),
    ("FP16", "0a92994d729ff76a58f692d3028ca1b64b145d91",
     "pkg:github/maratyszcza/fp16@0a92994d729ff76a58f692d3028ca1b64b145d91", "MIT", {"osx-arm64"},
     ("deps", "fp16", "0a92994d729ff76a58f692d3028ca1b64b145d91"), None),
    ("psimd", "072586a71b55b7f8c584153d223e95687148a900",
     "pkg:github/maratyszcza/psimd@072586a71b55b7f8c584153d223e95687148a900", "MIT", {"osx-arm64"},
     ("deps", "psimd", "072586a71b55b7f8c584153d223e95687148a900"), None),
]

NOTICE = f"""OCR assets for fluree-doc-parse (`fdoc --ocr oar-ocr --models DIR`).

pp-ocrv6-small/   PP-OCRv6 small text detection and recognition, ONNX exports
                  published by PaddlePaddle (huggingface.co/PaddlePaddle/
                  PP-OCRv6_small_det_onnx and PP-OCRv6_small_rec_onnx).
                  Licence: Apache-2.0 (pp-ocrv6-small/LICENSE). dict.txt is
                  the recognizer's character list, from the inference.yml
                  published with it.
onnxruntime/      ONNX Runtime {ORT_VERSION}, Microsoft's build from the
                  Microsoft.ML.OnnxRuntime NuGet package. Licence: MIT
                  (onnxruntime/LICENSE); third-party notices in
                  onnxruntime/ThirdPartyNotices.txt.

SHA256SUMS lists every file. fdoc loads nothing whose hash is not the one it
was built with.
sbom.cdx.json declares the runtime, the libraries compiled into it, and the
models (CycloneDX 1.5) for an SBOM:
a scan of a binary's Rust dependencies does not see what it loads at run time.
Built by tools/ocr-assets/build.py.
"""


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def fetch(cache, name):
    url, want = SOURCES[name]
    path = os.path.join(cache, name)
    if not os.path.exists(path):
        print(f"downloading {url}", file=sys.stderr)
        tmp = path + ".part"
        with urllib.request.urlopen(url, timeout=600) as r, open(tmp, "wb") as f:
            shutil.copyfileobj(r, f)
        os.replace(tmp, path)
    data = open(path, "rb").read()
    if sha256(data) != want:
        sys.exit(f"{name}: sha256 {sha256(data)} is not the published {want} ({url})")
    return data


def character_list(yml):
    """The `character_dict:` list of a PaddleOCR inference.yml, one entry per line.

    The file is YAML, but this list is a flat block sequence of scalars, so it is
    read here rather than adding a YAML dependency; the pinned hash of the result
    catches any entry read wrong.
    """
    lines = yml.decode("utf-8").splitlines()
    start = next(i for i, l in enumerate(lines) if l.strip() == "character_dict:")
    indent = len(lines[start]) - len(lines[start].lstrip())
    out = []
    for l in lines[start + 1:]:
        stripped = l.lstrip()
        if not stripped.startswith("- ") or len(l) - len(stripped) < indent:
            break
        v = stripped[2:]
        if len(v) >= 2 and v[0] == v[-1] == "'":
            v = v[1:-1].replace("''", "'")
        elif len(v) >= 2 and v[0] == v[-1] == '"':
            v = v[1:-1].encode("utf-8").decode("unicode_escape").encode("latin-1").decode("utf-8")
        out.append(v)
    return ("\n".join(out) + "\n").encode("utf-8")


def deps_ref(url):
    """The tag or commit a deps.txt source-archive URL names, or None (a release asset)."""
    m = (re.search(r"/archive/refs/tags/(.+?)\.(?:zip|tar\.gz)$", url)
         or re.search(r"/archive/([0-9a-f]{40})[./]", url)
         or re.search(r"/tar\.gz/refs/tags/(.+)$", url))
    return m.group(1) if m else None


def version_key(v):
    return [int(p) if p.isdigit() else p for p in re.split(r"[.-]", v)]


def check_embedded(cache):
    """Check every EMBEDDED row's version against the pinned ONNX Runtime sources."""
    config = json.loads(fetch(cache, "ort-vcpkg-configuration.json"))
    if config["default-registry"]["baseline"] != VCPKG_BASELINE:
        sys.exit(f"vcpkg-configuration.json names baseline {config['default-registry']['baseline']}, pinned {VCPKG_BASELINE}")
    deps = {}
    for line in fetch(cache, "ort-deps.txt").decode("utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            name, url, _ = line.split(";")
            deps[name] = deps_ref(url)
    manifest = json.loads(fetch(cache, "ort-vcpkg.json"))
    baseline = json.loads(fetch(cache, "vcpkg-baseline.json"))["default"]
    overrides = {o["name"]: o["version"].split("#")[0] for o in manifest.get("overrides", [])}
    minimum = {}
    for d in manifest["dependencies"]:
        if isinstance(d, str):
            minimum.setdefault(d, None)
        elif not d.get("host"):
            minimum[d["name"]] = d.get("version>=")
    for name, _, _, _, _, (kind, key, want), _ in EMBEDDED:
        if kind == "deps":
            got = deps.get(key)
        elif key not in minimum:
            got = None
        elif key in overrides:
            got = overrides[key]
        else:
            got = max(filter(None, [baseline[key]["baseline"], minimum[key]]), key=version_key)
        if got != want:
            sys.exit(f"EMBEDDED {name}: {kind} {key} resolves to {got}, the row says {want}")


def embedded(platform):
    return [e for e in EMBEDDED if e[4] is None or platform in e[4]]


def sbom(sums):
    """A CycloneDX 1.5 fragment declaring what the directory ships: ONNX Runtime
    (one component per platform library, with its package URL and CPE, so a
    vulnerability scanner matches it to advisories, each with the EMBEDDED
    libraries compiled into it as nested components) and the models.

    Deterministic, with no timestamp and a serial number derived from the files,
    so the same inputs give the same bytes and the same SHA256SUMS.
    """
    hashes = dict((rel, h) for h, rel in sums)
    ort_purl = f"pkg:nuget/Microsoft.ML.OnnxRuntime@{ORT_VERSION}"
    components = []
    for platform, (_, rel, _) in RUNTIMES.items():
        if rel not in hashes:
            continue
        components.append({
            "type": "library",
            "bom-ref": f"onnxruntime-{platform}",
            "supplier": {"name": "Microsoft"},
            "name": "onnxruntime",
            "version": ORT_VERSION,
            "purl": ort_purl,
            "cpe": f"cpe:2.3:a:microsoft:onnxruntime:{ORT_VERSION}:*:*:*:*:*:*:*",
            "licenses": [{"license": {"id": "MIT"}}],
            "hashes": [{"alg": "SHA-256", "content": hashes[rel]}],
            "properties": [{"name": "fluree:file", "value": rel}, {"name": "fluree:platform", "value": platform}],
            "externalReferences": [{"type": "distribution", "url": SOURCES["onnxruntime.nupkg"][0]}],
            "components": [{
                "type": "library",
                "bom-ref": f"onnxruntime-{platform}/{name}",
                "name": name,
                "version": version,
                "purl": purl,
                "licenses": [{"license": {"id": licence}}],
                "properties": [{"name": "fluree:version-source",
                                "value": "onnxruntime cmake/deps.txt" if kind == "deps" else "vcpkg"}],
            } for name, version, purl, licence, _, (kind, _, _), _ in embedded(platform)],
        })
    for name, repo, rev, rel in [
        ("PP-OCRv6_small_det", "PP-OCRv6_small_det_onnx", DET_REV, "pp-ocrv6-small/det.onnx"),
        ("PP-OCRv6_small_rec", "PP-OCRv6_small_rec_onnx", REC_REV, "pp-ocrv6-small/rec.onnx"),
    ]:
        components.append({
            "type": "machine-learning-model",
            "bom-ref": name,
            "supplier": {"name": "PaddlePaddle"},
            "name": name,
            "version": rev,
            "purl": f"pkg:huggingface/PaddlePaddle/{repo}@{rev}",
            "licenses": [{"license": {"id": "Apache-2.0"}}],
            "hashes": [{"alg": "SHA-256", "content": hashes[rel]}],
            "properties": [{"name": "fluree:file", "value": rel}],
            "externalReferences": [{"type": "distribution", "url": f"https://huggingface.co/PaddlePaddle/{repo}/tree/{rev}"}],
        })
    components.append({
        "type": "data",
        "bom-ref": "PP-OCRv6_small_rec-dict",
        "supplier": {"name": "PaddlePaddle"},
        "name": "PP-OCRv6_small_rec character dictionary",
        "version": REC_REV,
        "licenses": [{"license": {"id": "Apache-2.0"}}],
        "hashes": [{"alg": "SHA-256", "content": hashes["pp-ocrv6-small/dict.txt"]}],
        "properties": [{"name": "fluree:file", "value": "pp-ocrv6-small/dict.txt"}],
    })
    seed = "".join(f"{h} {rel}\n" for h, rel in sorted(sums, key=lambda s: s[1]))
    digest = hashlib.sha256(seed.encode()).hexdigest()
    serial = f"urn:uuid:{digest[:8]}-{digest[8:12]}-5{digest[13:16]}-{'89ab'[int(digest[16], 16) % 4]}{digest[17:20]}-{digest[20:32]}"
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": serial,
        "version": 1,
        "metadata": {
            "component": {"type": "data", "bom-ref": "fdoc-ocr-assets", "name": "fdoc OCR assets"},
            "tools": {"components": [{"type": "application", "name": "fluree-doc-parse tools/ocr-assets/build.py"}]},
        },
        "components": components,
    }
    return (json.dumps(bom, indent=2, sort_keys=True) + "\n").encode("utf-8")


def write(out, rel, data, want, sums):
    got = sha256(data)
    if got != want:
        sys.exit(f"{rel}: sha256 {got}, pinned {want}")
    path = os.path.join(out, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    sums.append((got, rel))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--cache", default=os.path.join(os.path.expanduser("~"), ".cache", "fdoc-ocr-assets"))
    ap.add_argument("--platforms", default=",".join(RUNTIMES), help="comma-separated: " + ", ".join(RUNTIMES))
    a = ap.parse_args()
    platforms = [p for p in a.platforms.split(",") if p]
    unknown = [p for p in platforms if p not in RUNTIMES]
    if unknown:
        sys.exit(f"unknown platform(s): {', '.join(unknown)}")
    os.makedirs(a.cache, exist_ok=True)
    # A file left from another build would sit in the directory unlisted in
    # SHA256SUMS and undeclared in the SBOM.
    if os.path.isdir(a.out) and os.listdir(a.out):
        sys.exit(f"{a.out} is not empty: give a new or empty directory")
    os.makedirs(a.out, exist_ok=True)

    sums = []
    write(a.out, "pp-ocrv6-small/det.onnx", fetch(a.cache, "det.onnx"), MODELS["pp-ocrv6-small/det.onnx"], sums)
    write(a.out, "pp-ocrv6-small/rec.onnx", fetch(a.cache, "rec.onnx"), MODELS["pp-ocrv6-small/rec.onnx"], sums)
    write(a.out, "pp-ocrv6-small/dict.txt", character_list(fetch(a.cache, "rec.yml")), MODELS["pp-ocrv6-small/dict.txt"], sums)
    write(a.out, "pp-ocrv6-small/LICENSE", fetch(a.cache, "apache-2.0.txt"), SOURCES["apache-2.0.txt"][1], sums)
    fetch(a.cache, "onnxruntime.nupkg")  # checks the package's hash
    nupkg = zipfile.ZipFile(os.path.join(a.cache, "onnxruntime.nupkg"))
    check_embedded(a.cache)
    for p in platforms:
        inner, rel, want = RUNTIMES[p]
        data = nupkg.read(inner)
        for name, _, _, _, _, _, marker in embedded(p):
            if marker and marker not in data:
                sys.exit(f"{rel}: {marker!r} not found, though EMBEDDED declares {name} in it")
        write(a.out, rel, data, want, sums)
    for name in ("LICENSE", "ThirdPartyNotices.txt"):
        data = nupkg.read(name)
        rel = f"onnxruntime/{name}"
        write(a.out, rel, data, sha256(data), sums)
    write(a.out, "NOTICE", NOTICE.encode("utf-8"), sha256(NOTICE.encode("utf-8")), sums)
    doc = sbom(sums)
    write(a.out, "sbom.cdx.json", doc, sha256(doc), sums)
    with open(os.path.join(a.out, "SHA256SUMS"), "w") as f:
        for h, rel in sorted(sums, key=lambda s: s[1]):
            f.write(f"{h}  {rel}\n")
    print(f"{len(sums)} files -> {a.out}", file=sys.stderr)
    loaded = [(rel, h) for h, rel in sorted(sums, key=lambda s: s[1])
              if rel.startswith("onnxruntime/") and "/" in rel[len("onnxruntime/"):] or rel in MODELS]
    partial = set(platforms) != set(RUNTIMES)
    print("PINNED in crates/fluree-doc-vision/src/assets.rs, for the files fdoc loads"
          + (" (only the platforms built here: keep the others' rows)" if partial else "") + ":",
          file=sys.stderr)
    for rel, h in loaded:
        print(f'    ("{rel}", "{h}"),', file=sys.stderr)


if __name__ == "__main__":
    main()
