# over

A git-based file overlay manager (dotfiles as overlays)

## Installation

```sh
brew install noirbizarre/tap/over   # Homebrew (macOS/Linux)
yay -S over-bin                     # AUR (prebuilt)
cargo binstall over                 # not on crates.io — binstall only
```

Or download a binary for your platform from the
[latest release](https://github.com/noirbizarre/over/releases/latest).
See the [README](https://github.com/noirbizarre/over#installation) for the
full list of install channels and supported platforms.

## Usage

```bash
over --help
```

`over`'s `git` subcommand group integrates with git workflows — see
[Usage](usage/index.md). For a walkthrough and two complete overlay-repository
layouts, see the [README's Quickstart](https://github.com/noirbizarre/over#quickstart)
and [Examples](examples.md).
