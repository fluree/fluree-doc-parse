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

A Rust toolchain (2021 edition). That is the whole list for the deterministic
tier, which is what runs unless you explicitly wire the model tiers.

The model tiers are **not** part of this binary. `fdoc` consumes their output
through files rather than calling them, so nothing here needs a GPU, a network
or a Python runtime — see [Wiring the escalation
tiers](../integration/escalation-tiers.md).

## Shell completions

```bash
fdoc completions zsh  > ~/.zfunc/_fdoc
fdoc completions bash > /etc/bash_completion.d/fdoc
```

See [`fdoc completions`](../cli/completions.md).

## Cold start

There is no model to load and no cache to warm, so the first document costs
the same as the thousandth — the deterministic tier measures ~7 ms per
document. This is the property that makes it viable in a Lambda-style
environment where a model container is not.
