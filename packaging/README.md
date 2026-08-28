# Packaging

Templates for the distribution channels that are not driven by
`cargo publish`. All three are rendered and pushed by workflows that run on
`release: published` — after gh-ship has undrafted the release, so the URLs
they bake in already resolve.

| Path | Channel | Workflow |
| --- | --- | --- |
| `aur/over/` | AUR, built from the release source tarball | `.github/workflows/aur.yaml` |
| `aur/over-bin/` | AUR, prebuilt binary (x86_64, aarch64) | `.github/workflows/aur.yaml` |
| `homebrew/over.rb` | `noirbizarre/homebrew-tap` | `.github/workflows/homebrew.yaml` |

Both AUR packages and the Homebrew formula install **both** binaries —
`over` and `git-over` — from the same source: `over` and `git-over` are not
alternatives, `git-over` is a companion Git subcommand.

## The placeholder contract

The templates are not valid as they stand: the workflows substitute
`@VERSION@` and the `@SHA256*@` placeholders from the published release
assets.

| Placeholder | Filled from |
| --- | --- |
| `@VERSION@` | the tag (no `v` prefix — the tag *is* the version) |
| `@SHA256@` | the GitHub-generated source archive for the tag |
| `@SHA256_X86_64@`, `@SHA256_AARCH64@` | `over_<version>_linux-amd64.tar.gz` / `over_<version>_linux-arm64.tar.gz` |
| `@SHA256_DARWIN_ARM64@`, `@SHA256_DARWIN_AMD64@` | `over_<version>_darwin-arm64.tar.gz` / `over_<version>_darwin-amd64.tar.gz` |
| `@SHA256_LINUX_AMD64_MUSL@` | `over_<version>_linux-amd64-musl.tar.gz` |
| `@SHA256_LICENSE@` (`over-bin` only) | the `LICENSE` file at the tag (the release archives carry no licence file) |

Checksums are always computed from the downloaded asset itself, never read
from a `.sha256` file published beside it: a mismatch between the two must
not be able to reach users.

Nothing else in these templates may hardcode a version: adding an asset
means adding both a placeholder and the substitution that fills it, and
`homebrew.yaml`/`aur.yaml` fail if any placeholder survives rendering.

## Renaming or removing a release asset

The templates address assets by name, so `.github/workflows/publish-
release.yaml` and these files change together: each archive is
`over_<version>_<asset>.<tar.gz|zip>`, produced by that workflow's "Stage
the asset" step, and carries **no leading directory** — only `over` and
`git-over` sit at its root today. The archive intentionally carries no man
pages or completions: completions are generated at install/package time by
running the extracted binary, and there is no `man` subcommand to generate
a page from yet.

## One-off setup

Only the credentials are manual. The pkgbases and the formula create
themselves on the first run — reusing the same GitHub App, AUR account and
`noirbizarre/homebrew-tap` already used by `git-tpl`/`git-wipe`.

### AUR

An `aur` environment on this repository holding a single secret,
`AUR_SSH_PRIVATE_KEY`: the private half of an SSH key registered on the AUR
account that maintains the two pkgbases. Kept out of the `release`
environment on purpose — a key that can push to the AUR has no business
sitting next to the GitHub App credentials.

The AUR creates a pkgbase on its first push, so the workflow imports both
packages itself as long as the names are free and the key belongs to the
account claiming them.

### Homebrew

A `homebrew` environment on this repository holding `TAP_TOKEN`, a
fine-grained token with `contents: write` on `noirbizarre/homebrew-tap` and
nothing else. The workflow creates `Formula/over.rb` on the first push.

## Re-running a failed publish

Both workflows are idempotent — they compare the staged index and exit
early when nothing changed — so a failed leg can simply be replayed:

```sh
gh workflow run aur.yaml -f tag=X.Y.Z
gh workflow run homebrew.yaml -f tag=X.Y.Z
```

## Testing a change

`aur.yaml` builds every package before pushing it, so a broken PKGBUILD
fails the workflow rather than reaching users. To check one locally,
substitute the placeholders against an already published release and run:

```sh
cd packaging/aur/over-bin
makepkg -si --noconfirm
namcap PKGBUILD ./*.pkg.tar.zst
```

For the formula, render it and hand it to brew directly:

```sh
brew install --formula ./packaging/homebrew/over.rb
brew test over
brew audit --strict --online over
```
