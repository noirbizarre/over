<h1 align="center">
  <img src="images/over-text.svg" alt="Over">
</h1>

Over manages files as **overlays**: directories in a Git-backed source repository
whose contents are applied to a **target** directory. Descriptors configure where
files go and how they appear there. By default, Over links individual files.

Wondering why Over exists or whether it fits your workspace? Read the [FAQ](faq.md),
including its [comparison with other tools](faq.md#alternatives).

<p align="center">
  <img src="images/demo.gif" alt="Over composing, previewing, and applying dotfile overlays">
</p>

## Getting started

### Install Over

Choose one installation channel:

```sh
brew install noirbizarre/tap/over   # Homebrew (macOS/Linux)
paru -S over-bin                    # Arch Linux AUR (prebuilt)
```

Or download a binary from the
[latest release](https://github.com/noirbizarre/over/releases/latest).
See [all installation options](https://github.com/noirbizarre/over#installation)
for supported platforms. The walkthrough below uses Git and a POSIX shell.

For development containers, use the [Over Dev Container Feature](devcontainer.md)
to install Over and apply an overlay during container creation.

### Create your first overlay

Start in temporary directories so you can see the result without changing your
existing dotfiles. This creates a source repository and an empty target:

```sh
export OVER_HOME="$(mktemp -d)"
export OVER_TARGET="$(mktemp -d)"
mkdir -p "$OVER_HOME/apps/hello"
git init "$OVER_HOME"
echo 'overlays = ["apps/*"]' > "$OVER_HOME/over.toml"
echo 'Hello from Over!' > "$OVER_HOME/apps/hello/greeting.txt"
```

The **root descriptor**, `$OVER_HOME/over.toml`, configures the repository and
discovers `apps/hello` through `overlays = ["apps/*"]`. This overlay inherits root
policy without a local descriptor. An optional **overlay descriptor** at
`$OVER_HOME/apps/hello/over.toml` can customize it later. `greeting.txt` is the file
to apply; descriptors themselves are never deployed.

### Preview, apply, inspect

```sh
over apply apps/hello --dry-run
over apply apps/hello
over status apps/hello
cat "$OVER_TARGET/greeting.txt"
```

The preview reports a link to create. Apply creates
`$OVER_TARGET/greeting.txt` pointing to `$OVER_HOME/apps/hello/greeting.txt`. Status should
report it as applied, and reading the target prints `Hello from Over!`.

`OVER_HOME` selects the **source** repository, just like `--home`; `OVER_TARGET`
selects the **target** directory, just like `--target`, changing what `~` means
in destinations. Explicit flags take precedence over these environment variables.
Without these overrides, the defaults are `~/.dotfiles` and your home directory.
Explicit `apps/hello` selection avoids contextual inference and missing-selection
errors; overlay selection does not open an interactive chooser.

To reverse this example, run:

```sh
over unapply apps/hello
unset OVER_HOME OVER_TARGET
```

When you already have an Over repository, [`over init`](usage/init.md) clones or
validates it; applying remains a separate step. To keep a new setup, create its
files under `~/.dotfiles` and version them with Git.

For the bigger picture, the [FAQ](faq.md) explains overlays, composed workstations,
and how Over fits alongside Git and other tools.

## Examples

Grow from one overlay to a reusable setup with the [complete examples](examples.md):

- [One flat overlay](examples.md#one-flat-overlay): the simplest dotfiles layout.
- [Compose a host](examples.md#compose-a-host): reuse app overlays with `uses`.
- [Inherit shared policy](examples.md#inherit-shared-policy): root and intermediate
  descriptors provide defaults; nearer descriptors refine them.
- [Several destinations](examples.md#one-workspace-several-destinations): route
  directories to different targets.
- [Editable checkouts](examples.md#edit-ordinary-files-with-checkout): stage and
  record changes without target symlinks.

For exact fields and flags, use [Configuration](configuration.md) and
[Commands](usage/index.md).
