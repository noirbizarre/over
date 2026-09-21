# Examples

Two complete overlay-repository layouts, from the simplest thing that works
to a composed, multi-host setup. Both use generic names — no real hostnames,
usernames, organizations, or secrets.

See [Configuration](configuration.md) for `default_overlay`/templating
details, and [`over apply`](usage/apply.md) for how composition (`uses:`) is
reconciled.

## Example 1 — Flat, single overlay

The minimum viable setup: `OVER_HOME` itself *is* one overlay, no
subdirectories, no `uses:`. The README's Quickstart and demo start one
step past this, with a small `apps/`-per-tool layout that also shows
composition (`uses:`) and templating — see Example 2 below for that
pattern at scale.

```text
~/dotfiles/
├── over.toml          # target = "~"
├── .gitconfig
├── .bashrc
└── .config/
    └── starship.toml
```

```toml
# ~/dotfiles/over.toml
target = "~"
```

With only one overlay and no `default_overlay` configured
(see [Configuration](configuration.md)), a bare `over apply` still prompts
interactively to confirm it — having a single candidate doesn't skip the
prompt on its own, only an explicit `NAME` or a configured
`default_overlay` does. Pass `.` (the overlay's path, here the repository
root itself) to skip the prompt in scripts: `over apply .` symlinks every
entry straight into `$HOME`. Growing beyond one tool means splitting this
into subdirectories, each with its own descriptor — see example 2.

## Example 2 — Composed, multi-host layout

One overlay per app, bundled into stacks and per-OS overlays, pulled
together by per-host root overlays, plus a `workspaces/` family that uses
`git:` to clone related repositories and a `target:` template. This shows
diamond `uses:` composition, per-OS `install:` blocks, templating, and
`default_overlay` resolving per machine.

```text
~/dotfiles/
├── over.toml                # default_overlay = "hosts/{{ machine.hostname }}"
├── apps/
│   ├── git/                 # over.yaml: .gitconfig + install: per-OS packages
│   ├── tmux/
│   └── starship/            # over.yaml: links: templated by a theme variable
├── stacks/
│   ├── dev/                 # over.yaml: uses: [apps/git, apps/tmux, ...]
│   └── python/
├── os/
│   ├── linux/                # uses: [stacks/dev]
│   ├── archlinux/            # uses: [os/linux]  install.archlinux: [...]
│   └── macos/                # uses: [stacks/dev]  install.brew: [...]
├── desktop/
│   └── hyprland/             # uses: [apps/*]  install.archlinux: [...]
├── workspaces/
│   └── myproject/            # target: ~/Workspaces/myproject
│                              # git: { common/lib: git@github.com:me/lib.git }
└── hosts/
    ├── laptop/                # uses: [os/macos, workspaces/myproject]
    └── workstation/           # uses: [os/archlinux, desktop/hyprland]
```

```yaml
# ~/dotfiles/hosts/laptop/over.yaml
uses:
  - os/macos
  - workspaces/myproject
```

```toml
# ~/dotfiles/workspaces/myproject/over.toml
target = "~/Workspaces/myproject"

[git]
"common/lib" = "git@github.com:me/lib.git"
```

```toml
# ~/dotfiles/over.toml
default_overlay = "hosts/{{ machine.hostname }}"
```

With that root config, a bare `over apply` on any machine resolves to the
right host overlay automatically, which then pulls in its OS, desktop, and
workspace overlays transitively — `over status`/`over diff` narrow to that
same default once it's configured (pass `-a`/`--all` to see every overlay
regardless).

A `target:` doesn't have to be a fixed path either — it can be rendered from
the overlay's own name, letting one descriptor apply to every overlay
beneath it without repeating the field. A directory with its own
`over.toml` is only listed as its own overlay when none of its
subdirectories have one too — the more specific overlay always wins — so
`workspaces/over.toml` below never becomes an overlay in its own right, only
an ancestor contributing `target` to whichever `workspaces/*` overlay
doesn't set its own:

```toml
# ~/dotfiles/workspaces/over.toml
target = "~/Workspaces/{{ overlay.name | split('/') | last }}"
```
