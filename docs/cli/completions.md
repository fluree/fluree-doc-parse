# fdoc completions

Generate shell completions.

```bash
fdoc completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`.

## Install

```bash
# zsh
fdoc completions zsh > ~/.zfunc/_fdoc
# ensure ~/.zfunc is on $fpath, then: compinit

# bash
fdoc completions bash > /etc/bash_completion.d/fdoc

# fish
fdoc completions fish > ~/.config/fish/completions/fdoc.fish
```

The output goes to stdout; redirect it wherever your shell looks.
