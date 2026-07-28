# CLI reference

```
fdoc <command> [options]
```

| command | what it does |
|---|---|
| [`convert`](convert.md) | documents → Markdown, XHTML, JSON, DoCO JSON-LD or text |
| [`forms`](forms.md) | AcroForm fields with their filled values |
| [`triage`](triage.md) | which pages would escalate, and why |
| [`dev`](dev.md) | pipeline internals, for debugging (**unstable**) |
| [`completions`](completions.md) | shell completions |

`triage` is also available as `route`.

## Global flags

| flag | effect |
|---|---|
| `-v`, `--verbose` | per-document timing on stderr |
| `-q`, `--quiet` | suppress non-essential output |
| `--no-color` | disable color (also respects `NO_COLOR`) |
| `--version` | version |
| `-h`, `--help` | help; works per-subcommand too |

`--verbose` and `--quiet` conflict.

## Exit codes

`0` on success, non-zero when a document fails to convert. In batch mode the
exit code reflects whether any input failed; successful documents are still
written.

## Stability

`convert`, `forms` and `triage` are compatibility surfaces. **`dev` is not** —
it exposes intermediate layout state and its output may change in any release.

## Environment

Several environment variables configure the model tiers and some diagnostics;
see [Environment variables](../reference/env-vars.md).
