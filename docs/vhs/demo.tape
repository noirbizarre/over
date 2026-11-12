# Run `mise run demo` so the recording uses the freshly built local binary.
Output docs/images/demo.gif

Set Shell "bash"
# Keep emoji coverage when overriding VHS's default font fallback chain.
Set FontFamily "JetBrains Mono, Symbols Nerd Font"
Set FontSize 20
Set Width 1000
Set Height 620
Set Padding 20
# VHS 0.11 names this theme with a lowercase identifier.
Set Theme "lovelace"
Set TypingSpeed 45ms
Set PlaybackSpeed 1.0

# Literal paths avoid depending on shell expansion in VHS's environment values.
Env HOME "/tmp/over-demo-home"
Env OVER_HOME "/tmp/over-demo-home/.dotfiles"
Env OVER_TARGET "/tmp/over-demo-home"
Env XDG_CONFIG_HOME "/tmp/over-demo-home/.config"
Env XDG_DATA_HOME "/tmp/over-demo-home/.local/share"
Env XDG_CACHE_HOME "/tmp/over-demo-home/.cache"
Env XDG_STATE_HOME "/tmp/over-demo-home/.local/state"
Env PROMPT "\e[34;2m~$\e[0m "
Env LC_ALL "C.UTF-8"
Env GIT_CONFIG_GLOBAL "/dev/null"
Env GIT_CONFIG_NOSYSTEM "1"

Hide
# Bash uses PS1 rather than PROMPT; ignore inherited prompt hooks as well.
Type `unset PROMPT_COMMAND; export PS1="$PROMPT"; export PATH="$PWD/target/debug:$PATH"`
Enter
# Reset only this dedicated home, so repeat recordings start from the same state.
Type `rm -rf -- "$HOME" && mkdir -p "$OVER_HOME/apps/git" "$OVER_HOME/hosts/demo" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"`
Enter
Type `git -c init.defaultBranch=main init --template= "$OVER_HOME"`
Enter

# Heredocs avoid VHS interpreting printf's escape sequences as keystrokes.
Type `cat > "$OVER_HOME/over.toml" <<'CFG'`
Enter
Type 'overlays = ["apps/*", "hosts/*"]'
Enter
Type "CFG"
Enter
Type `cat > "$OVER_HOME/apps/git/.gitconfig" <<'CFG'`
Enter
Type "[user]"
Enter
Type "    name = Ada Lovelace"
Enter
Type "    email = ada@example.com"
Enter
Type "CFG"
Enter
Type `cat > "$OVER_HOME/hosts/demo/over.toml" <<'CFG'`
Enter
Type 'uses = ["apps/git"]'
Enter
Type "CFG"
Enter

# Leave ~/.gitconfig absent so preview and apply show a genuine state change.
Type `cd "$HOME" && clear`
Enter
# VHS trims terminal-line whitespace before matching the prompt.
Wait /\~\$$/
Show
Sleep 1s

Type "over list --tree"
Enter
Wait /\~\$$/
Sleep 3s

Type "cat ~/.dotfiles/hosts/demo/over.toml"
Enter
Wait /\~\$$/
Sleep 3s

# Keep the final result visible without scrolling away the applied summary.
Type "over apply hosts/demo"
Enter
Wait /\~\$$/
Sleep 4s

Type "cat ~/.gitconfig"
Enter
Wait /\~\$$/
Sleep 4s

Type "over status hosts/demo"
Enter
Wait /\~\$$/
Sleep 5s

Hide
# Leave the sandbox before removing it; teardown must never appear on camera.
Type `cd /tmp && rm -rf -- "$HOME"`
Enter
Wait /\~\$$/
