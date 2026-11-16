# Dev Containers

Copy Over from its published image into your development image, then use normal
Over commands during container creation. Docker handles the binary download and
architecture selection; no Over installer or lifecycle wrapper is needed.

## Add Over to your image

Create `.devcontainer/Dockerfile`:

```dockerfile
ARG OVER_VERSION=0.4.0
FROM ghcr.io/noirbizarre/over:${OVER_VERSION} AS over

FROM mcr.microsoft.com/devcontainers/base:bookworm
COPY --from=over /usr/local/bin/over /usr/local/bin/over
```

The final image remains your chosen development environment. Only the Over
executable is copied; its distroless filesystem and entrypoint are not inherited.
The example development base supplies Git, a shell, certificates and compatible
runtime libraries. Other bases must supply those prerequisites themselves.
Published Linux binaries support amd64/arm64 with glibc, including Debian 12+
and Ubuntu 24.04+; they do not support Alpine's musl runtime.

Choose a published Over tag through `OVER_VERSION`, or pin the source image by
digest for reproducibility. Use `latest` only when you intend to follow a floating
image. Docker selects the source image for the target architecture, so do not
force the Over stage to the build host's architecture when cross-building.

## Clone a repository

Create `.devcontainer/devcontainer.json`, replacing the repository and overlay
names with your own:

```json
{
    "build": {
        "dockerfile": "Dockerfile"
    },
    "remoteUser": "vscode",
    "postCreateCommand": "over init --no-config -- https://github.com/user/dotfiles.git && over apply --no-prompt -- container/base container/rust"
}
```

`postCreateCommand` runs as the lifecycle user after container creation, when
mounts and that user's credentials are available. Over's default source home is
therefore `/home/vscode/.dotfiles`, not `/root/.dotfiles`. Do not put repository
credentials in the Dockerfile or committed configuration.

`--no-config` explicitly declines creation of missing root configuration during
unattended initialization; existing configuration remains authoritative. Use
`--mode checkout` instead when you want to create missing checkout-mode
configuration. See [`over init`](commands/init.md#noninteractive-setup) for these
and other initialization choices.

The `&&` keeps initialization and application sequential and stops on the CLI's
error. Do not split dependent operations into object-valued lifecycle commands,
which can run concurrently. To initialize without applying, omit the second
command; to install only the binary, omit `postCreateCommand`.

Multiple overlay names are ordinary separate CLI arguments in **one** apply
operation, not a comma-separated option. Quote names normally if they contain
spaces. Over owns selection, dependency composition and conflict handling; see
[`over apply`](commands/apply.md).

## Use a mounted repository

Keep the same Dockerfile and mount an existing checkout into the lifecycle user's
source home:

```json
{
    "build": {
        "dockerfile": "Dockerfile"
    },
    "remoteUser": "vscode",
    "mounts": [
        "source=${localEnv:HOME}/.dotfiles,target=/home/vscode/.dotfiles,type=bind"
    ],
    "postCreateCommand": "over apply --no-prompt -- container/base container/rust"
}
```

This source expression assumes a Unix-like host with `HOME` available. A sibling
checkout can instead use `source=${localWorkspaceFolder}/../dotfiles`. The bind
source is a host/Docker-daemon path; the target must match the container user's
actual source home. Ensure that user can read the checkout and write its targets.

For a custom source location, set normal `OVER_HOME` through `containerEnv` to an
absolute container path, or pass `--home` to the CLI commands. This selects the
source repository, not the overlay deployment target.

If the mounted checkout needs initialization or its init hooks, prepend
`over init --no-config &&` to the apply command. Initialization may modify
repository-local configuration and run hooks; it is not a read-only validation.

## Existing files, retries and updates

`over apply --no-prompt` fails on conflicts instead of prompting or forcing
replacement. Inspect the CLI diagnostic, resolve the conflict, then rerun the
same command in the container. For example:

```sh
over apply --no-prompt -- container/base container/rust
```

Repeating `over init` reuses an existing checkout without implicitly pulling,
resetting local changes or approving origin changes. Init/apply retain their
normal repository hook behavior. Use the ordinary CLI/Git commands for updates,
encryption and deliberate origin changes; see [`over init`](commands/init.md) and
[destructive operations and recovery](usage/safety.md).

Post-create commands do not run on every start or attach. Rebuilding the
container reruns the creation lifecycle; manual retries use the same CLI commands
shown in your configuration.

## Migrate from the former Feature

Remove the Over entry from `features`, including references to
`ghcr.io/noirbizarre/features/over:0`,
`ghcr.io/noirbizarre/over/features/dotfiles:1`, or
`ghcr.io/noirbizarre/over/dotfiles:1`. Add the Dockerfile and lifecycle configuration
above, then rebuild the container.

| Former Feature option | Direct configuration |
| --- | --- |
| `version` | Select the Over image tag or digest in the Dockerfile. |
| `repository` | Pass the Git source to `over init`. |
| `overlay` | Pass separate names to `over apply`, replacing the comma-separated list. |
| `overHome` | Use the CLI's default, `OVER_HOME`, or `--home`. |

Replace retries through `/usr/local/share/over/setup.sh` with your normal
init/apply commands. The Feature is no longer maintained or published; existing
registry packages are not deleted or redirected.
