# Dev Containers

Add the Over Feature to your existing development image. It installs a bundled
binary and optionally initializes a repository and applies overlays as the
container's lifecycle user. No custom Dockerfile is required.

## Consume a read-only host checkout

For disposable containers, prepare your dotfiles on the host and mount the
**complete checkout read-only**. Set `readOnly` so Over deploys without modifying
the source or recording persistent Over state. Omit `repository`:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "mounts": [
        "source=${localEnv:HOME}/.dotfiles,target=/home/vscode/.dotfiles,type=bind,readonly"
    ],
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
            "overHome": "/home/vscode/.dotfiles",
            "readOnly": true,
            "overlay": "container/base,container/rust"
        }
    }
}
```

The helper runs one
`over apply --read-only --no-prompt -- container/base container/rust` command.
It does not initialize, pull, or unlock the source. If encryption is configured,
unlock the checkout on the host first and make its existing private Git state
readable to the lifecycle user. Missing, locked, stale, or unsafe unlock state is
an error; Over never tries to repair it in this mode.
Keep the host checkout stable during apply: lock-free reads do not coordinate
with simultaneous host edits or key rotation.

Deployment targets remain writable. Ordinary links point into the mount;
templates and virtual-checkout contents are published directly at their targets,
without XDG-backed artifacts or managed-checkout associations. This mode creates
no persistent Over bookkeeping and has no historical base for sync/commit or
stale-file deletion. Source links need the mount for their entire lifetime.
See [`over apply --read-only`](commands/apply.md#consume-a-read-only-source).

**Hooks and declared Git dependencies still run normally**, including their
subprocesses, filters, and network access. The read-only guarantee applies to
Over's own source and operational-state writes, not arbitrary executable
configuration. A read-only filesystem mount also enforces source protection for
those commands. Target files and target Git exclusions are deployment effects.

Use ephemeral home/targets for this workflow; no persistent XDG volume is needed
when they are disposable too. It is not a way to manage persistent virtual
checkouts while discarding their historical state.

The Feature contains both Linux amd64 and arm64 binaries and installs the matching
one during image construction. Supported bases are Debian 12+ and Ubuntu 24.04+
with Bash and glibc. The installer checks for Git and a CA certificate bundle;
it runs `apt-get` only if a prerequisite is missing. If common-utils or Git
Features were selected, `installsAfter` orders Over after them without requiring
those Features. There is no crane bootstrap, separate binary download or
Docker-image extraction.

Application runs during `postCreateCommand`, as `vscode` in
this example. Over's normal home resolution therefore defaults to
`/home/vscode/.dotfiles`. `overHome` selects the **source repository**, not the
deployment target. Its mount target must match that path. The lifecycle user
must be able to traverse/read the source and write the overlay targets.

## Develop dotfiles with persistent writable mounts

For intentional edits and Git commits inside the container, mount the checkout
read-write and use regular apply (`readOnly` defaults to false). Omit `repository`
here too. **Edits, apply/absorb operations, and commits affect the host checkout.**

Preserve three distinct storage locations:

```json
{
    "image": "mcr.microsoft.com/devcontainers/base:bookworm",
    "remoteUser": "vscode",
    "containerEnv": {
        "XDG_STATE_HOME": "/home/vscode/.local/state"
    },
    "onCreateCommand": "sudo mkdir -p \"$XDG_STATE_HOME\" && sudo chown vscode:vscode \"$XDG_STATE_HOME\"",
    "mounts": [
        "source=over-home-${devcontainerId},target=/home/vscode,type=volume",
        "source=${localEnv:HOME}/.dotfiles,target=/home/vscode/.dotfiles,type=bind",
        "source=over-state-${devcontainerId},target=/home/vscode/.local/state,type=volume"
    ],
    "features": {
        "ghcr.io/noirbizarre/features/over:0": {
            "overHome": "/home/vscode/.dotfiles",
            "overlay": "hosts/container"
        }
    }
}
```

The home volume preserves deployed files and user data; the dedicated XDG volume
preserves `$XDG_STATE_HOME/over`, including virtual-checkout ownership and
base/checkpoint associations. Ensure both volumes are owned by the lifecycle
user before setup runs. With this image, an `onCreateCommand` can initialize a
fresh state volume with `sudo chown vscode:vscode "$XDG_STATE_HOME"`; do not
recursively chown the shared dotfiles bind mount. Keep the same state path and
permissions on every recreation.

The checkout, deployed targets, and state need compatible lifetimes. If targets
survive but state is lost, their previous ownership/base cannot be recovered
from current source HEAD or deployed files. Reinitialization may establish a
new association, but cannot recover that historical base. Preserve/restore the
matching state instead.

Regular `over apply`, `over unlock`, `over lock`, editing, and Git commits work
with their usual permissions and credentials. Encrypted repositories must be
unlocked explicitly when needed; the Feature does not automate unlock. Perform
deliberate source updates with Git rather than expecting setup to pull.

## Clone a remote repository instead

Use explicit `repository` initialization when the host checkout cannot be
mounted, such as cloud Dev Containers or remote Docker daemons:

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

This invokes `over init` followed by regular `over apply --no-prompt`.
Initialization uses normal unattended policy; a repository without root
configuration requires an explicit configuration choice. See
[`over init`](commands/init.md#noninteractive-setup). Preserve home, source,
and XDG state if deployment targets must survive recreation, as above.

Provide credentials through the lifecycle user's runtime Git/SSH facilities;
never put them in Feature options or bake them into image layers. Mounted source
consumption needs no access to its Git remote or repository credentials.

## Complete checkouts, permissions and platforms

- Mount working-tree contents **and `.git`**. For linked worktrees, `.git` is a
  pointer: mount the actual gitdir and common Git directory at reachable paths
  too, preserving the paths referenced by the pointer files. Working-tree
  contents alone are insufficient. Avoid relocating a host worktree whose
  absolute Git paths are unavailable inside the container.
- Host unlock state resides in private Git metadata and must be available under
  the correct user/ownership permissions. A readable checkout does not imply a
  readable key. Do not weaken key permissions to share it between unrelated
  users; align container UID/GID with the host owner.
- Normal Git filters require their referenced configuration, executables,
  identity/key mounts, and credentials. A read-only mount cannot support a
  filter that needs to install state there. Prepare/unlock on the host, or use
  the writable workflow. Virtual outputs retain Over's canonical committed-blob
  semantics; arbitrary worktree smudge transformations are not added to them.
- `${localEnv:HOME}` assumes a Unix-like host. On Windows, choose the appropriate
  host checkout path. Bind sources belong to the Docker daemon's host; cloud or
  remote daemons cannot automatically bind your laptop's checkout.
- Linux UID/GID mapping and Docker Desktop file sharing affect access. Ensure
  the lifecycle user has source traversal/read permissions and target/state
  write permissions. Named-volume ownership must also be initialized correctly.

## Standalone Docker image

The image runs the `over` CLI, includes Git and CA certificates, and defaults to
help. It never silently applies or clones. Add any tools required by your hooks,
external filters, or authentication setup to a derived image.

For ephemeral deployment, use a read-only source and disposable writable target
home. This Unix example uses the host UID/GID so existing private key permissions
remain valid, and discards targets with the container:

```sh
docker run --rm --user "$(id -u):$(id -g)" \
  --env HOME=/tmp/over-home --env XDG_STATE_HOME=/tmp/over-state \
  --mount "type=bind,source=$HOME/.dotfiles,target=/dotfiles,readonly" \
  ghcr.io/noirbizarre/over:latest \
  --home /dotfiles apply --read-only --no-prompt -- container/base
```

Choose overlays whose targets follow this `HOME` or explicitly set `--target`;
absolute target rules still apply. The mount is the source, not the target.

For persistent dotfiles development, prepare writable host-owned target/state
directories and use three mounts (these directory mounts can also be replaced
by appropriately owned persistent volumes):

```sh
mkdir -p "$HOME/.over-container-home" "$HOME/.over-container-state"
docker run --rm --user "$(id -u):$(id -g)" \
  --env HOME=/home/over --env XDG_STATE_HOME=/var/lib/over-state \
  --mount "type=bind,source=$HOME/.dotfiles,target=/dotfiles" \
  --mount "type=bind,source=$HOME/.over-container-home,target=/home/over" \
  --mount "type=bind,source=$HOME/.over-container-state,target=/var/lib/over-state" \
  ghcr.io/noirbizarre/over:latest \
  --home /dotfiles apply --no-prompt -- container/base
```

Reuse the same mounts/environment for later commands. For example, replace the
CLI arguments with `--home /dotfiles unlock` or `--home /dotfiles lock`.
For a shell to edit and commit, add `-it --entrypoint /bin/sh` before the image
name and replace the Over arguments with `-i`; use `git -C /dotfiles ...` with your runtime Git
identity/credentials. Changes affect the host source directly. No credentials
are required simply to consume either mounted checkout.

## Options and versions

| Option | Default | Meaning |
| --- | --- | --- |
| `repository` | `""` | Git source passed to `over init`; empty skips initialization. |
| `overlay` | `""` | Ordered comma-separated names; empty skips application. |
| `overHome` | `""` | Explicit source-home override; empty uses normal CLI home resolution. |
| `readOnly` | `false` | Pass `--read-only` to apply; requires an existing checkout and empty `repository`. |

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

- **Missing/invalid checkout:** check the bind source exists on the daemon host,
  its target matches `overHome`, and Git metadata was included. Empty directories
  created for a misspelled mount source are not Over repositories.
- **Incomplete worktree:** make the referenced gitdir/common directory available,
  or mount a standalone full checkout instead.
- **Permission denied:** check parent-directory traversal, UID/GID, lifecycle
  user, and target/state-volume ownership. Read-only source does not mean
  read-only deployment targets.
- **Locked/unavailable key:** unlock or repair on the host and remount all
  required metadata with compatible ownership. Read-only setup never unlocks.
- **Recorded target ownership:** ephemeral apply cannot reuse persistent managed
  checkouts; choose disposable targets or use regular apply with retained state.
- **Hook/filter failure on a read-only source:** prepare its required state on
  the host or intentionally choose the writable workflow.

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
