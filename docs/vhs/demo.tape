# Regenerates docs/images/demo.gif — run `mise run demo` rather than `vhs`
# directly, so `target/debug/over` is rebuilt first and always reflects the
# current source rather than a stale or globally-installed binary.

Output docs/images/demo.gif

Set Shell "bash"
# "Noto Color Emoji" as a fallback for `over`'s 📦/✨/📁 status glyphs —
# JetBrains Mono alone has no emoji coverage, and overriding FontFamily drops
# VHS's own default fallback chain that would normally provide one. Chromium's
# canvas text rendering may still draw them as placeholder boxes rather than
# full color glyphs; either way the text around them stays legible.
Set FontFamily "JetBrains Mono, Noto Color Emoji"
Set FontSize 16
Set Width 1200
Set Height 680
Set Padding 20
Set Theme "Dracula"
Set TypingSpeed 35ms
Set PlaybackSpeed 1.0

# Everything in this block builds a throw-away sandbox ($HOME and OVER_HOME
# both point inside a mktemp dir) and pre-seeds two overlays that would be
# tedious to hand-type on camera (a `uses:` composition and a templated
# `target:`). None of it is shown — Hide/Show only toggles what's recorded,
# the commands still run for real against the sandbox.
Hide

# A short PS1 keeps the recording readable — the real prompt would be a
# `mktemp -d` path, not something worth putting on screen. LC_ALL=C keeps
# `ls -la`'s date column in a fixed, locale-independent format rather than
# whatever locale happens to be configured on the machine regenerating this.
Type "export PS1='$ ' LC_ALL=C DEMO=$(mktemp -d)"
Enter
Type "export HOME=$DEMO/home OVER_HOME=$DEMO/dotfiles"
Enter
Type "mkdir -p $HOME $OVER_HOME"
Enter

# A pre-existing dotfile for `over add` to absorb, exactly like adopting a
# config file you already have rather than starting from nothing. A heredoc
# rather than `printf '...\n...'`: VHS processes `\n`/`\t` escapes in
# double-quoted Type strings, which would send a real newline mid-command.
Type "cat > $HOME/.gitconfig <<'CFG'"
Enter
Type "[user]"
Enter
Type "    name = Ada Lovelace"
Enter
Type "    email = ada@example.com"
Enter
Type "CFG"
Enter

# `hosts/demo` composes `apps/git` via `uses:` — the pattern behind a real
# per-machine root overlay. `workspaces/demo`'s `target:` is rendered from
# `overlay.name` itself, showing the templating available to every field
# that goes through `exec::templates::render_string`.
Type "mkdir -p $OVER_HOME/hosts/demo $OVER_HOME/workspaces/demo"
Enter
Type "cat > $OVER_HOME/hosts/demo/over.toml <<'CFG'"
Enter
Type 'target = "~"'
Enter
Type 'uses = ["apps/git"]'
Enter
Type "CFG"
Enter
Type "cat > $OVER_HOME/workspaces/demo/over.toml <<'CFG'"
Enter
# Backticks, not quotes: this line mixes both a TOML double-quoted string and
# a MiniJinja single-quoted filter argument — VHS's own quoting docs say to
# escape both kinds of quotes with backticks rather than nesting them.
Type `target = "~/Workspaces/{{ overlay.name | split('/') | last }}"`
Enter
Type "CFG"
Enter

Type "export PATH=$PWD/target/debug:$PATH"
Enter
Type "cd $DEMO && clear"
Enter
Show

Type "over new apps/git ~ --force"
Enter
Sleep 800ms

Type "over add ~/.gitconfig --overlay apps/git --force"
Enter
Sleep 600ms
Type "ls -la --color=always ~/.gitconfig"
Enter
Sleep 1s

Type "over list --tree"
Enter
Sleep 1500ms

Type "over show hosts/demo"
Enter
Sleep 1500ms

Type "over apply hosts/demo --dry-run --no-prompt"
Enter
Sleep 1500ms

Type "over apply hosts/demo --no-prompt"
Enter
Sleep 1500ms

Type "over apply workspaces/demo --dry-run --no-prompt"
Enter
Sleep 1500ms

Type "over status"
Enter
Sleep 2500ms
