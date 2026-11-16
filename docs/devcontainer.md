# Dev Containers

Add the Over Feature to your existing development image. It installs a bundled
binary and optionally initializes a repository and applies overlays as the
container's lifecycle user. No custom Dockerfile is required.

## Clone a repository

Add this to `.devcontainer/devcontainer.json`, replacing the repository and overlay
names with your own:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
            "repository": "https://github.com/user/dotfiles.git",
            "overlay": "container/base,container/rust"
        }
    }
}
```

The Feature contains both Linux amd64 and arm64 binaries and installs the matching
one during image construction. Supported bases are Debian 12+ and Ubuntu 24.04+
with Bash and glibc. The installer checks for Git and a CA certificate bundle;
it runs `apt-get` only if a prerequisite is missing. If common-utils or Git
Features were selected, `installsAfter` orders Over after them without requiring
those Features. There is no crane bootstrap, separate binary download or
Docker-image extraction.

Initialization and application run during `postCreateCommand`, as `vscode` in
this example. Over's normal home resolution therefore defaults to
`/home/vscode/.dotfiles`. Repository credentials must be available to that user;
do not put credentials in Feature options.

The helper invokes `over init` and one
`over apply --no-prompt -- container/base container/rust` command. Over owns
repository reuse, overlay composition, conflict handling and diagnostics. Init
uses the CLI's normal unattended policy: for example, a repository without root
configuration requires an explicit configuration choice before using this setup.
See [`over init`](commands/init.md#noninteractive-setup).

## Use a mounted repository

Omit `repository` to use an existing checkout. For example:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "mounts": [
        "source=${localEnv:HOME}/.dotfiles,target=/home/vscode/.dotfiles,type=bind"
    ],
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
            "overlay": "hosts/container"
        }
    }
}
```

The mount source assumes a Unix-like host with `HOME` available. Its container
target must match Over's source home for the lifecycle user. Ensure that user can
read the checkout and write the overlay targets.

## Options and versions

| Option | Default | Meaning |
| --- | --- | --- |
| `repository` | `""` | Git source passed to `over init`; empty skips initialization. |
| `overlay` | `""` | Ordered comma-separated names; empty skips application. |
| `overHome` | `""` | Explicit source-home override; empty uses normal CLI home resolution. |

Names in `overlay` are trimmed and passed literally as separate arguments to one
apply operation. Blank list elements fail before initialization. Shared
dependencies and repeated names are handled by Over; see [`over apply`](commands/apply.md).

An explicit `overHome` supports leading `~` or `~/` relative to the lifecycle
user's home. Otherwise the CLI uses `OVER_HOME`, if provided, or its normal
`~/.dotfiles` default. This selects the source repository, not the deployment
target. Both `repository` and `overlay` empty install only the binary.

The **Feature reference selects the bundled Over version**. Pin a full published
Feature version for reproducibility, or use a major/minor reference to follow its
published updates. There is no independent
binary `version` option, and no mutable image is fetched during installation.

## Errors, retries and updates

The lifecycle helper passes through the CLI's errors and exit status.
`--no-prompt` refuses conflicts instead of authorizing replacement. Resolve the
reported problem, then retry:

```sh
/usr/local/share/over/setup.sh
```

Normal Over commands work too. Repeating initialization reuses an existing
checkout without pulling or resetting local changes; repository hooks retain
their normal behavior. Use Git/Over directly for deliberate updates and policy
decisions. See [`over init`](commands/init.md) and
[destructive operations and recovery](usage/safety.md).

Setup runs after creation, not on every start or attach. Rebuilding the container
installs the selected Feature and repeats that lifecycle.

## Migrate an existing configuration

The Feature remains at `ghcr.io/noirbizarre/features/over`. If an older
configuration uses `version`, remove that option and select the desired release
in the Feature reference instead. Floating Feature references track published
Feature releases, not unreleased main-branch images.

An unspecified `overHome` now delegates to the CLI, including its normal
`OVER_HOME` environment variable. Explicit home overrides and comma-separated
overlay selections remain supported.

Replace legacy `ghcr.io/noirbizarre/over/features/dotfiles:1` or
`ghcr.io/noirbizarre/over/dotfiles:1` references with the current Feature namespace.
See the [Feature contributor guide](https://github.com/noirbizarre/over/tree/main/devcontainer)
for packaging and testing.
