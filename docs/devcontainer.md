# Dev Containers

Install Over and apply selected dotfiles overlays as the Dev Container lifecycle
user. Supported platforms are Linux amd64 and arm64 on Debian 12+ and Ubuntu
24.04+ images with Bash and glibc.

## Clone a repository

Add this to `.devcontainer/devcontainer.json`, replacing the source and overlay:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
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

## Combine reusable overlays

Keep the `overlay` option and separate multiple names with commas:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
            "repository": "https://github.com/user/dotfiles.git",
            "overlay": "container/base,container/rust,container/project-tools"
        }
    }
}
```

Existing single-name configurations continue to work without migration. Whitespace
around names is trimmed; commas cannot be part of a name. Empty elements such as
`"base,,rust"`, `"base,"` or whitespace-only input fail with the element position.
An entirely empty option skips application.

The list uses the same flat-union composition as an overlay's `uses`:

- Traverse each name in list order, visiting its dependencies first, depth-first.
  Shared dependencies and repeated names participate once, at their first visit.
- Each overlay retains its own target. File ownership conflicts are errors in
  either order; later names do **not** override earlier files.
- Validate every reference and the combined ownership before deploying any files.
  An invalid later name cannot leave earlier overlays deployed.
- Merge metadata after repository defaults in dependency-first traversal order:
  mappings merge recursively, while later scalar/list values replace earlier ones.
  A listed dependency already visited through another name is not merged again.
  A single distinct selected root retains its existing highest metadata precedence.
- Run repository hooks once, then participant hooks in dependency order, only after
  every participant successfully materializes. Multiple distinct roots give repository
  hooks no selected `overlay` or `OVER_OVERLAY_*` environment variables; participant
  hooks still receive their own overlay identity and target.

This is one `over apply --no-prompt -- container/base container/rust container/project-tools`
operation. A `version` override must support multi-name `over apply` to use a list;
older binaries fail rather than applying each name independently.

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
        "ghcr.io/noirbizarre/features/over:0": {
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
| `overlay` | `""` | Ordered, comma-separated overlay names; empty skips application, including repository-default overlays. |
| `repository` | `""` | Git source passed to `over init`; empty uses an existing/mounted repository. |
| `overHome` | `"~/.dotfiles"` | Source checkout; `~` and `~/` resolve against the lifecycle user's home. |
| `version` | Matching feature release | Override the tag of the Over image at `ghcr.io/noirbizarre/over`. |

The Feature version matches the Over release and installs that binary version
by default. The `:0` reference tracks Over's current major version; pin the full
version of a published Feature release for reproducibility. Use `:1` once Over reaches
1.x. Floating major/minor references advance with releases.

Set `"version": "latest"` to explicitly follow the latest binary image, or choose
another published image tag. An override intentionally decouples the binary from
the Feature release.

The Feature treats received option values literally except comma separation and
surrounding whitespace trimming in `overlay`, and leading-tilde expansion in `overHome`.
The current Dev Container CLI writes options into
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
/usr/local/share/over/setup.sh
```

For an intentional replacement, run the CLI yourself after reviewing its
effects, for example `over apply hosts/container --dry-run`, followed by an
explicitly authorized application. See [destructive operations and recovery](usage/safety.md)
for the meaning of `--force` and the limits of recovery. Apply is not a
transaction: successful earlier steps may remain after a later conflict.

Repeating setup reuses the existing checkout and already-applied files. It does
not implicitly pull updates, reset local changes or replace an existing origin.
Use normal Git/Over commands to update a checkout deliberately. Initialization
and application retain the CLI's repository hook behavior; hooks must themselves
be safe to repeat. See [`over init`](usage/init.md).

Missing repositories, unknown overlays, failed initialization and unavailable
binary image versions fail with diagnostic context. Installation failures occur
during the image build; setup failures occur during `postCreateCommand`. The
Feature does not automatically reapply on every start or attach; rerun the
setup script when needed. Rebuilding reruns the post-create lifecycle.

## Migrate an existing configuration

Replace `ghcr.io/noirbizarre/over/features/dotfiles:1` (or the older
`ghcr.io/noirbizarre/over/dotfiles:1`) with `ghcr.io/noirbizarre/features/over:0`.
The old Feature's `:1` was an independent version; the new tags follow Over's
release numbering. Existing registry references are not redirected. Rebuild the
container after changing the reference; the retry script now lives at
`/usr/local/share/over/setup.sh`.

For contributor testing and publication details, see the
[feature source README](https://github.com/noirbizarre/over/tree/main/devcontainer).
