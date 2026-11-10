# Over Dotfiles Dev Container Feature

Install Over and apply a selected dotfiles overlay as the Dev Container lifecycle
user. Supported platforms are Linux amd64 and arm64 on Debian 12+ and Ubuntu
24.04+ images with Bash and glibc.

## Clone a repository

Add this to `.devcontainer/devcontainer.json`, replacing the source and overlay:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "features": {
        "ghcr.io/noirbizarre/over/features/dotfiles:1": {
            "repository": "https://github.com/user/dotfiles.git",
            "overlay": "hosts/container"
        }
    }
}
```

The Feature installs the binary during the image build as root, provisioning
Git, curl, CA certificates and tar when needed. It downloads a temporary crane
binary to extract Over from its published OCI image. Build-time network access
to Debian/Ubuntu package repositories, GitHub and GHCR is required.

The `postCreateCommand` initializes the repository and applies the overlay as
the lifecycle user. `~/.dotfiles` therefore means `/home/vscode/.dotfiles` in
this example, not `/root/.dotfiles`. Repository credentials, if needed, must be
available to that user during initialization. Feature options are baked into
the image; do not put credentials in them.

## Use a mounted repository

Omit `repository` to use an existing checkout. For example, mount the host's
dotfiles at an explicit container path:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "mounts": [
        "source=${localEnv:HOME}/.dotfiles,target=/home/vscode/.dotfiles,type=bind"
    ],
    "features": {
        "ghcr.io/noirbizarre/over/features/dotfiles:1": {
            "overlay": "hosts/container"
        }
    }
}
```

Ensure the lifecycle user can read the checkout and write the overlay's target
paths. Overlay configuration still controls deployment targets; `overHome`
selects the **source**, not the target.

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `overlay` | `""` | Overlay to apply; empty skips application, including repository-default overlays. |
| `repository` | `""` | Git source passed to `over init`; empty uses an existing/mounted repository. |
| `overHome` | `"~/.dotfiles"` | Source checkout; `~` and `~/` resolve against the lifecycle user's home. |
| `version` | `"latest"` | Tag of the Over image at `ghcr.io/noirbizarre/over`. |

The `:1` in the Feature reference selects the **Feature version**. The `version`
option independently selects the **Over binary image version**. Pin both to
published versions when you need reproducible builds.

The Feature treats received option values literally except the documented
leading-tilde expansion. The current Dev Container CLI writes options into
double-quoted shell assignments before invoking the Feature: literal `$`,
backticks, backslashes and double quotes need shell escaping as well as JSON
escaping at that boundary. For example, use `"overHome": "~/dot\\$files"` for
a directory named `dot$files` with CLI 0.88.0. Use absolute paths for predictable custom locations;
relative paths resolve from the lifecycle workspace directory. With both
`repository` and `overlay` empty, setup is a no-op. Setting only `repository`
initializes the checkout without applying files.

## Existing files, retries and updates

Application uses `over apply --no-prompt`. A conflicting file, directory or
foreign symlink fails setup and is not forcibly replaced. Inspect the CLI
diagnostic, reconcile or move the conflicting content, then retry:

```sh
/usr/local/share/over-dotfiles/setup.sh
```

For an intentional replacement, run the CLI yourself after reviewing its
effects, for example `over apply hosts/container --dry-run`, followed by an
explicitly authorized application. See [destructive operations and recovery](../../docs/usage/safety.md)
for the meaning of `--force` and the limits of recovery. Apply is not a
transaction: successful earlier steps may remain after a later conflict.

Repeating setup reuses the existing checkout and already-applied files. It does
not implicitly pull updates, reset local changes or replace an existing origin.
Use normal Git/Over commands to update a checkout deliberately. Initialization
and application retain the CLI's repository hook behavior; hooks must themselves
be safe to repeat. See [`over init`](../../docs/usage/init.md).

Missing repositories, unknown overlays, failed initialization and unavailable
binary image versions fail with diagnostic context. Installation failures occur
during the image build; setup failures occur during `postCreateCommand`. The
Feature does not automatically reapply on every start or attach; rerun the
setup script when needed. Rebuilding reruns the post-create lifecycle.

## Test and publish

With mise, Docker, Git, Python 3, Node.js and network access available:

```sh
mise install devcontainer-cli
mise run test:features
```

Run these commands from the repository root. The CLI follows `latest` in
`mise.toml`, with its exact version recorded in `mise.lock`. Set `DEVCONTAINER` to an
absolute CLI executable path if needed. The suite stages specification-compliant
local Features and runs real `devcontainer up`/`exec` commands against minimal
Debian images, with a non-root lifecycle user and deterministic Git fixtures.
It tests actual published binary downloads, clone/mount behavior, repeat setup,
literal options, conflict preservation and expected failures. Test containers,
anonymous volumes and temporary workspaces are removed; Docker's image/build
cache remains. CI runs the suite natively on amd64 and arm64 and gates publishing
on both architectures passing.

The standard Dev Container publisher packages only this Feature directory,
creates collection metadata and publishes OCI Feature artifacts. Feature
versions are independent of Over releases, and already-published exact Feature
versions are immutable: changed contents require a new Feature version through
the project's release process.

Feature artifacts now live under `ghcr.io/noirbizarre/over/features`: update
older references from `ghcr.io/noirbizarre/over/dotfiles:1` to
`ghcr.io/noirbizarre/over/features/dotfiles:1`. The collection's `:latest` tag
must be separate from the Over binary image's `ghcr.io/noirbizarre/over:latest`
tag so publishing a Feature cannot replace the binary image with metadata.
