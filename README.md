<p align="center">
  <img src="docs/images/logo-text.svg" alt="over" width="640">
</p>

<p align="center"><strong>A git-based file overlay manager (dotfiles as overlays)</strong></p>

<p align="center">
  <a href="https://github.com/noirbizarre/over/actions/workflows/ci.yaml">
    <img src="https://github.com/noirbizarre/over/actions/workflows/ci.yaml/badge.svg" alt="CI">
  </a>
  <a href="https://codecov.io/gh/noirbizarre/over">
    <img src="https://codecov.io/gh/noirbizarre/over/graph/badge.svg" alt="Codecov">
  </a>
  <img src="https://img.shields.io/github/v/release/noirbizarre/over" alt="Release">
  <a href="https://noirbizarre.github.io/over/">
    <img src="https://img.shields.io/badge/docs-noirbizarre.github.io-blue" alt="Documentation">
  </a>
  <img src="https://img.shields.io/github/license/noirbizarre/over" alt="License">
</p>

---

Over is a git-based file overlay manager that lets you define file overlays in
Git repositories, with support for nested references and installation
requirements. It is particularly well-suited for managing dotfiles.
It is inspired by tools like GNU Stow and Chezmoi but focuses on a Git-centric
workflow with flexible configuration and installation capabilities.

<p align="center">
  <img src="docs/images/demo.gif" alt="over creating, composing (uses:), and applying overlays" width="800">
</p>

## Installation

### Homebrew (macOS/Linux)

```sh
brew install noirbizarre/tap/over
```

### Arch Linux (AUR)

```sh
yay -S over-bin   # prebuilt binary
# or, to build from source:
yay -S over
```

### cargo-binstall

`over` isn't published to crates.io, so a plain `cargo install over` won't
work — but prebuilt binaries are available through
[`cargo-binstall`](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo binstall over
```

### Download a binary

Download a binary for your platform from the
[latest release](https://github.com/noirbizarre/over/releases/latest).

**Supported platforms:** Linux (amd64/arm64, glibc or musl) and macOS
(amd64/arm64 — Intel and Apple Silicon).

## Quickstart

Point `over` at an overlay repository — a plain directory, versioned with
git — via `--home`/`OVER_HOME`, then create an overlay and add a file to it:

```console
$ over new apps/git ~ --force
Created overlay directory apps/git (toml)
Overlay apps/git created targeting /home/you

$ over add ~/.gitconfig --overlay apps/git --force

$ over list --tree
dotfiles
└── apps
    └── git
```

`over add` moves `~/.gitconfig` into `apps/git/` and replaces it with a
symlink back to that overlay. `over apply apps/git` re-applies it (or
reconciles it after editing the overlay's copy).

Overlays can also compose each other via `uses:`, and any field can be a
template rendered against the current overlay/machine:

```console
$ over show hosts/demo
hosts/demo
  root:   /home/you/dotfiles/hosts/demo
  target: ~
  uses:   apps/git

$ over apply hosts/demo --dry-run
📦 Applying overlay hosts/demo to /home/you
📦 Applying overlay apps/git to /home/you
Plan: 0 to create, 2 unchanged, 0 conflict(s), 0 to migrate, 0 migration(s) blocked, 0 to repair, 0 deferred

$ over apply workspaces/demo --dry-run
📦 Applying overlay workspaces/demo to /home/you/Workspaces/demo
Plan: 1 to create, 0 unchanged, 0 conflict(s), 0 to migrate, 0 migration(s) blocked, 0 to repair, 0 deferred
  📁 create directory: ~/Workspaces/demo
```

`workspaces/demo`'s `target` is
`~/Workspaces/{{ overlay.name | split('/') | last }}` — resolved from the
overlay's own name, one descriptor covering every overlay under
`workspaces/`. See the demo above for the full sequence in motion.

## Layout examples

**Flat, single overlay** — the simplest possible setup: no subdirectories
at all, `OVER_HOME` itself is the one overlay, with a single `target`.

```text
~/dotfiles/
├── over.toml       # target = "~"
├── .gitconfig
├── .bashrc
└── .config/
    └── starship.toml
```

With only one overlay and no `default_overlay` configured, a bare
`over apply` still prompts interactively to confirm it — pass `.` (the
overlay's path, here the repository root itself) to skip the prompt, e.g.
`over apply .`, which symlinks every entry straight into `$HOME`. The
Quickstart above starts one step past this, with a small `apps/`-per-tool
layout that also shows composition (`uses:`) and templating in the same
breath.

**Composed, multi-host layout** — one overlay per app, bundled into
per-stack/per-OS/per-desktop overlays, pulled together by per-host root
overlays, plus a `default_overlay` that resolves to the right host
automatically:

```text
~/dotfiles/
├── over.toml                 # default_overlay = "hosts/{{ machine.hostname }}"
├── apps/
│   ├── git/
│   ├── tmux/
│   └── starship/
├── stacks/
│   └── dev/                  # uses: [apps/git, apps/tmux, ...]
├── os/
│   ├── linux/
│   ├── archlinux/            # uses: [os/linux]       install.archlinux: [...]
│   └── macos/                # uses: [stacks/dev]     install.brew: [...]
├── desktop/
│   └── hyprland/             # uses: [apps/*]          install.archlinux: [...]
├── workspaces/
│   └── myproject/            # target: ~/Workspaces/myproject
│                              # git: { common/lib: git@github.com:me/lib.git }
└── hosts/
    ├── laptop/                # uses: [os/macos, workspaces/myproject]
    └── workstation/           # uses: [os/archlinux, desktop/hyprland]
```

See [`docs/examples.md`](docs/examples.md) for the full walkthrough of both
layouts, descriptor contents included.

## Why over?

| | over | GNU Stow | chezmoi | yadm |
|---|---|---|---|---|
| Unit of management | git repository (overlay) | plain directory ("package") | single git repo | single git repo (bare) |
| Composing multiple sources | `uses:` (overlay graph) | — | `.chezmoiexternal` (flat includes) | — |
| Templating | MiniJinja | — | Go `text/template` | Go `text/template` (via alt files) |
| Package-manager integration | `install:` per overlay, per OS | — | run scripts (`run_once_*`) | — |
| Materialization | symlink, partial file, or git checkout | symlink only | render + copy, or symlink | symlink |

over leans into a git-centric workflow where each overlay is its own
concern — independently versionable, composable into others, and able to
declare the packages it needs — rather than one monolithic dotfiles
repository.

## Usage

```bash
over --help
```

### Commands

| Command | Description |
|---------|-------------|
| `over add` | Add files or directories to an overlay |
| `over new` | Create a new overlay |
| `over list` (`ls`) | List known overlays |
| `over show` | Display details about an overlay |
| `over apply` | Apply a given overlay |
| `over lint` | Check overlays for configuration issues |
| `over doctor` | Check the environment and overlays for issues |
| `over completion` | Generate shell completion scripts |
| `over status` | Get the current repository/directory overlays status |
| `over diff` | Show differences between desired and actual overlay state |
| `over sync` | Reconcile a checkout-materialized overlay with its git source |
| `over unapply` | Remove a given overlay's own entries from the target |
| `over commit` | Commit local changes in a checkout-materialized overlay back to its source repository |
| `over log` | Show the git history behind a checkout-materialized overlay |

`over`'s `git` subcommand group integrates with git workflows:

| Command | Description |
|---------|-------------|
| `over git mount` | Mount the current git repository to an overlay |
| `over git add` | Add files from the current git repository to an overlay |
| `over git status` | Show overlay status for the current git repository |

See the [documentation](https://noirbizarre.github.io/over/) for the full
command reference, configuration format, and install-manager reference
(package managers, precedence rules, platform sections).

## Documentation

<https://noirbizarre.github.io/over/>

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
