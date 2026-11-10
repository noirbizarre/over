# `over init`

Initialize an Over repository from a Git source, or validate an existing checkout.

```text
Usage: over init [OPTIONS] [SOURCE]

Arguments:
  [SOURCE]  Clone from a Git URL or local repository path

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
      --set-origin   Set an existing checkout's canonical origin without prompting
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Bootstrap before SSH is configured

Declare the canonical remote in the repository's root `over.yaml`:

```yaml
repository: git@github.com:user/dotfiles.git
```

The TOML equivalent is `repository = "git@github.com:user/dotfiles.git"` in
`over.toml`. Then bootstrap using HTTPS or a local/removable-media Git repository:

```sh
over init https://github.com/user/dotfiles.git
# Or, on another machine:
over init /media/usb/dotfiles.git

# Applying is always a separate, explicit step:
over apply hosts/my-machine
```

Over clones the supplied source, validates the checkout and sets `origin` to the
canonical remote without connecting to it. SSH can therefore be configured by a
later apply. Without `repository`, origin keeps the clone source. Credentials
use the existing libgit2 credential-helper and SSH-agent support; init does not
manage passwords, SSH keys or agents, or render templates.

## Destination and validation

The destination is `--home`, then `OVER_HOME`, then `~/.dotfiles`. With no source,
`over init` validates that location, not the current directory. To validate a
checkout elsewhere, use `over --home /path/to/checkout init`.

The checkout must be non-bare and have exactly one root `over.toml`, `over.yaml`
or `over.yml` with valid configuration. Both flat single-overlay layouts and
root-declared overlay directories are supported. No fixed `overlays/` directory
is required. Init checks root schema without applying overlays or evaluating
machine-dependent templates; use `over lint` for broader overlay diagnostics.

The default used to be `~/.over`. Existing repositories are not automatically
moved: set `OVER_HOME=~/.over` or pass `--home ~/.over` to keep using one.

## Existing checkouts and retries

- Absent or ordinary empty destinations can be cloned into. Files, symlinks,
  bare repositories and nonempty unrelated directories are refused.
- Existing valid Over checkouts are reused without fetching or resetting, even
  when a source is supplied. Output makes clear that the source was not cloned.
- If origin already matches the canonical remote, rerunning preserves Git
  configuration but still runs post-init hooks. If no canonical remote is
  configured, existing remotes are preserved.
- Changing or creating an existing checkout's origin requires confirmation.
  Declining preserves it and returns an incomplete-initialization error. Use
  `over init --set-origin` to approve without prompting; non-interactive runs
  otherwise fail with instructions when confirmation would be needed.
- Dirty files, other remotes, refspecs and explicit origin push URLs are
  preserved. An explicit push URL still governs pushes and is reported.
- Clone, configuration and remote-write errors return a nonzero exit code with
  context. Any partial checkout is preserved. Repair an invalid descriptor or
  Git configuration and retry; for an incomplete clone, choose another
  destination or explicitly remove the failed checkout after inspecting it.

Initialization never applies overlays or runs their install commands. Finish
with the normal `over apply [NAME]` workflow when ready.

## Lifecycle hooks

Init can execute repository-provided commands: **only initialize repositories you
trust**. Root `hooks.post-clone` runs after a fresh clone is validated and its
canonical origin configured; root `hooks.post-init` then runs on both fresh and
existing checkouts. Both run before child overlay descriptors are needed. For
example, keep the root descriptor plaintext and configure:

```yaml
hooks:
  post-clone:
    - git-crypt unlock
  post-init:
    - ./scripts/ensure-local-setup
```

A failed hook returns a nonzero status and stops initialization. The checkout
remains intact. Retrying init on it runs post-init only, so recover failed
post-clone commands manually or put repeatable unlock/setup commands in post-init.
See [Lifecycle hooks](../configuration/root.md#lifecycle-hooks) for ordering, environment,
working directories, and failure semantics.
