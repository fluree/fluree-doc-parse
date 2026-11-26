# Install

## Build from source

```bash
git clone https://github.com/fluree/fluree-doc-parse
cd fluree-doc-parse
cargo build --release
```

The binary lands at `target/release/fdoc`. There is nothing else to install:
no models to download, no Python, no system PDF library. The dependency tree
is pure Rust and entirely permissively licensed (run `cargo tree` to confirm).

```bash
target/release/fdoc --version
target/release/fdoc convert --help
```

The release binary carries no symbol table, which keeps it a fifth smaller.
A panic still names the file and line it happened at, but a backtrace has no
function names: `RUST_BACKTRACE=1` prints no frames at all, and
`RUST_BACKTRACE=full` only addresses. To chase one, build with the names
kept, which is cargo's own release default:

```bash
CARGO_PROFILE_RELEASE_STRIP=debuginfo cargo build --release
```

(`=false` would keep the standard library's debug info as well, a binary
nearly a third larger than that.)

## Requirements

A Rust toolchain (2021 edition). That is the whole list for mode
`deterministic`, the default.

Mode `ocr` also needs the OCR assets, a directory of models and ONNX Runtime
built by a script in this repository ([the assets
directory](../cli/ocr.md#the-assets-directory)). Mode `vlm` also needs a model
reader: a Google Vertex AI key ([`fdoc config gemini`](../cli/config.md)), or
readings produced by a model you run yourself and handed over as files
([Model readings](../integration/escalation-tiers.md)). None of the three
needs a GPU or a Python runtime; see [modes](modes.md).

## Shell completions

```bash
fdoc completions zsh  > ~/.zfunc/_fdoc
fdoc completions bash > /etc/bash_completion.d/fdoc
```

See [`fdoc completions`](../cli/completions.md).

## Cold start

There is no model to load and no cache to warm, so the first document costs
the same as the thousandth — the deterministic tier measures ~8 ms per
document. This is the property that makes it viable in a Lambda-style
environment where a model container is not.
