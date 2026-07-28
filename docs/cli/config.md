# config

```bash
fdoc config gemini --credentials ~/sa-key.json   # set the deep reader up
fdoc config show                                 # what is set, and is it ready
fdoc config path                                 # which file is in effect
fdoc config init [--global]                      # write a commented template
fdoc config set <key> <value>                    # one key at a time
```

Only the deep reader is configured. Everything else about a parse is
deterministic and takes no settings — that is what makes a parse reproducible
on someone else's machine.

**With nothing configured, `fdoc` never reaches the network.** Naming a
provider is what turns escalation on.

## Setting up Google Vertex AI

You need a service-account key with the **Vertex AI User** role, from
`IAM & Admin > Service Accounts > Keys > Add key > JSON` in the Google Cloud
console.

```bash
fdoc config gemini --credentials ~/Downloads/sa-key.json
```

```
escalation configured in /Users/you/.config/fluree-doc-parse/config.toml
  provider gemini, model gemini-3-flash-preview, project my-project

`fdoc convert <pdf>` now escalates the pages that ask for it.
```

The key is read and checked here rather than at the first request, so a wrong
file is caught now and not halfway through a batch. The path is stored
absolute — a relative one would resolve against whatever directory the next
run started in.

`--project` overrides the project in the key; `--model` names a different
model.

## Where the config lives

Nearest first:

1. `./.fdoc/config.toml`, walking up from the working directory — so a project
   can pin its own settings
2. `$FDOC_HOME/config.toml`
3. the per-user config directory: `~/.config/fluree-doc-parse/config.toml` on
   Linux, `~/Library/Application Support/fluree-doc-parse/` on macOS

`fdoc config path` prints the one in effect. Written files are owner-only
(`0600`), because the file names the path to a private key.

## The file

```toml
[escalation]
enabled = true
provider = "gemini"
model = "gemini-3-flash-preview"
concurrency = 6

[escalation.gemini]
credentials = "/Users/you/sa-key.json"
project = "my-project"          # optional; read from the key when absent
```

| key | means |
|---|---|
| `enabled` | run the configured reader when no flag says otherwise |
| `provider` | `gemini` is the one this build implements |
| `model` | passed to the provider unchanged |
| `concurrency` | crops read at once |
| `on_column_doubt` | also escalate pages that read across their panels |
| `gemini.credentials` | path to the service-account JSON key |
| `gemini.project` | overrides the project named in the key |

## Pages that read across their panels

A page laid out as panels side by side under a full-width heading has columns
a page-global whitespace projection cannot see — the heading spans the gutters,
so the projection finds no gap and reading order runs across the panels:

```
The charities I support are extremely important to
What happens to our
me — how do I maximize
child's inheritance
```

`fdoc triage <file>` reports those pages as `COLUMN`, and they do **not**
escalate by default:

```
report  COLUMN  p7 2 column(s) found, 3 gutter(s) visible only in a band covering 57% of rows

COLUMN pages do not escalate by default — `fdoc config set escalation.on_column_doubt true`
```

The default is a measurement, not caution. Over a 200-document corpus of
reports and papers this flags 22 documents, gains less in total than the
heading signal alone, and makes five worse. On layout-heavy material — decks,
brochures, one-pagers — it marks exactly the pages that need it. Nothing on
the page separates the two populations, so it is a fact about a corpus and
lives in a corpus's configuration.

```bash
fdoc config set escalation.on_column_doubt true
```

Editing through `fdoc config set` preserves comments and any key this build
does not recognise, so a file can be hand-written and machine-edited in turn.

## Turning it on and off

| you run | what happens |
|---|---|
| nothing configured | deterministic, silent, no network |
| configured | escalates the pages that ask for it |
| `--no-escalate` | deterministic, whatever the config says |
| `--escalate`, nothing configured | **warns**, then parses deterministically |
| `enabled = false` | `--escalate` becomes the opt-in |

`--escalate` with no provider is not an error — the deterministic parse is
still the right output — but it is never silent, because a batch run under a
wrong assumption otherwise looks like a working one.

```
warning: --escalate was asked for but no provider is set
         run `fdoc config gemini --credentials <key.json>` to set one up
         parsing deterministically
```

Two paths deliberately never escalate: `--tier-results` supplies readings
someone already has, so paying to produce them again would be surprising; and
the hidden `fdoc md` / `json` / `xhtml` forms that benchmark adapters shell,
so a configured key on the machine that ran a benchmark cannot change the
number.

## What it costs

`fdoc triage <file>` prices a run before you make it — it reports what would
escalate and why, without sending anything. Over the 200-document evaluation
corpus, 113 documents ask for nothing at all.

`--pages` narrows what is *read*, not only what is printed, so inspecting one
page of a long deck costs one crop.
