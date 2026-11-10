<h1 align="center">
  <img src="images/over-text.svg" alt="Over">
</h1>

Over manages files as **overlays**: directories in a Git-backed source repository
whose contents are applied to a **target** directory. Descriptors configure where
files go and how they appear there. By default, Over links individual files.

## Getting started

### Install Over

Choose one installation channel:

```sh
brew install noirbizarre/tap/over   # Homebrew (macOS/Linux)
yay -S over-bin                    # Arch Linux AUR (prebuilt)
cargo binstall over                # Prebuilt binaries, not cargo install
```

Or download a binary from the
[latest release](https://github.com/noirbizarre/over/releases/latest).
See [all installation options](https://github.com/noirbizarre/over#installation)
for supported platforms. The walkthrough below uses Git and a POSIX shell.

### Create your first overlay

Start in a temporary directory so you can see the result without changing your
existing dotfiles. This creates a source repository and an empty target:

```sh
demo="$(mktemp -d)"
mkdir -p "$demo/source/apps/hello" "$demo/target"
git init "$demo/source"
printf 'format = "toml"\n' > "$demo/source/over.toml"
printf 'target = "~"\n' > "$demo/source/apps/hello/over.toml"
printf 'Hello from Over!\n' > "$demo/source/apps/hello/greeting.txt"
```

The **root descriptor**, `source/over.toml`, configures the repository. The
**overlay descriptor**, `source/apps/hello/over.toml`, configures `apps/hello`.
`greeting.txt` is the file to apply; descriptors themselves are never deployed.

### Preview, apply, inspect

```sh
over --home "$demo/source" apply apps/hello --root "$demo/target" --dry-run
over --home "$demo/source" apply apps/hello --root "$demo/target"
over --home "$demo/source" status apps/hello --root "$demo/target"
cat "$demo/target/greeting.txt"
```

The preview reports a link to create. Apply creates
`target/greeting.txt` pointing to `source/apps/hello/greeting.txt`. Status should
report it as applied, and reading the target prints `Hello from Over!`.

`--home` selects the **source** repository; `--root` changes what `~` means in
destinations. In daily use, they default to `~/.dotfiles` and your home directory.
Explicit `apps/hello` selection avoids an interactive overlay prompt.

To reverse this example, run:

```sh
over --home "$demo/source" unapply apps/hello --root "$demo/target"
```

When you already have an Over repository, [`over init`](usage/init.md) clones or
validates it; applying remains a separate step. To keep a new setup, create its
files under `~/.dotfiles` and version them with Git.

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
