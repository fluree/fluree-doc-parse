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
