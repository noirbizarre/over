# `over completion`

Generate shell completion scripts.

```
Usage: over completion [OPTIONS] <SHELL>

Arguments:
  <SHELL>  Shell to generate completions for [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

```bash
# Bash (add to ~/.bashrc)
over completion bash >> ~/.bashrc

# Zsh (add to ~/.zshrc)
over completion zsh >> ~/.zshrc

# Fish
over completion fish > ~/.config/fish/completions/over.fish

# PowerShell (add to profile)
over completion powershell >> $PROFILE
```
