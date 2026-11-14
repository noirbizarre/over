# `over init`

Initialize an Over repository from a Git source, or validate an existing checkout.

```text
Usage: over init [OPTIONS] [SOURCE]

Arguments:
  [SOURCE]  Clone from a Git URL or local repository path

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
      --mode <MODE>  Create a missing root configuration with this repository default [possible values: checkout, symlink]
      --no-config    Continue without creating missing root configuration
      --set-origin   Set an existing checkout's canonical origin without prompting
      --unlock       Explicitly unlock repository encryption during initialization
  -i, --identity <IDENTITY>  Use a standard Age identity when unlocking
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Choose a repository default

When the checkout has no root `over.toml`, `over.yaml`, or `over.yml`, interactive
init explains that configuration is missing and offers to create `over.toml`.
Accepting opens a materialization choice:

- **Virtual checkout (recommended)** is selected by default. Apply creates
  ordinary target files from committed source `HEAD`, with changes recorded back
  through the checkout workflow.
- **Symlink** links target files to the source, so editing a target edits its source.

For example, selecting virtual checkout writes this minimal root configuration:

```toml
mode = "checkout"
```

Init reports the created path and repository default. This policy is inherited
by overlays; nearer descriptors and ordered rules retain their precedence.
The file is not staged or committed automatically. Review and commit it to share
the default with future checkouts. See [Materialization](../configuration/materialization.md).

Existing configuration is preserved without setup questions, including descriptors
that omit `mode`. Their implicit fallback remains symlink. Adding a default does
not convert deployed files or symlinks, migrate virtual checkouts, apply overlays,
or authorize encryption unlock. Existing deployments require an explicit migration.

Declining creation continues without a root descriptor. Only discovered child
overlays are available, with the implicit symlink fallback unless their own policy
overrides it. You can create the root descriptor later with
`over init --mode checkout` or `over init --mode symlink`.

### Noninteractive setup

Missing configuration requires an explicit decision when stdin or stderr is not
a terminal. Without one, init fails with guidance instead of prompting:

```sh
over init https://github.com/user/dotfiles.git --mode checkout
over init https://github.com/user/dotfiles.git --mode symlink
over init https://github.com/user/dotfiles.git --no-config
```

`--mode` and `--no-config` are mutually exclusive. `--mode` only selects the default
when creating missing configuration; it is not a per-command override. Repeating
it with existing configuration preserves that file and reports that its policy
remains authoritative. `--no-config` likewise preserves existing configuration.
Invalid or multiple root descriptors must be repaired; these flags cannot replace
them. Origin changes and encryption unlock retain their separate consent flags.

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

The checkout must be non-bare. Existing root configuration must be exactly one
valid `over.toml`, `over.yaml`, or `over.yml`; missing configuration follows the
setup flow above. Both flat single-overlay layouts and
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

Initialization never applies overlays or runs install hooks. Finish
with the normal `over apply [NAME]` workflow when ready.

## Repository encryption

Init configures repository-local Over filters before protected refresh or hooks.
For repositories with access metadata, interactive init offers unlock with
confirmation defaulting to No. It unwraps the repository key once, then persists
it in private Git common administrative storage. This key survives terminal
closure and reboot until `over lock`; filters never prompt.

Fresh noninteractive clones remain locked unless explicitly requested with `--unlock`.
Use `--unlock --identity <standard-age-identity-file>` for explicit identity access.
Repeat `--identity` for multiple files. Encrypted identity files and plugin-backed
identities can request a password, PIN, touch or confirmation; unattended unlock
requires an available credential that needs no interaction. See
[Age and hardware-key compatibility](crypt.md#users-and-keys).
Declining unlock keeps ciphertext. If an encrypted repository declares bootstrap
hooks, initialization stops before executing any of them and explains how to
unlock. Retrying an existing checkout runs post-init, not post-clone; put
repeatable setup in post-init or recover skipped post-clone steps explicitly.
An already-unlocked checkout remains unlocked and repairs incomplete ciphertext
refreshes without another credential prompt, preserving existing plaintext edits.
A manual clone can be unlocked later with
`over unlock`. Bootstrap descriptors and encryption metadata must remain readable
while locked. See [repository encryption](crypt.md).

## Lifecycle hooks

Init can execute repository-provided commands: **only initialize repositories you
trust**. Root `hooks.post-clone` runs after a fresh clone is validated and its
canonical origin configured; root `hooks.post-init` then runs on both fresh and
existing checkouts. Both run before child overlay descriptors are needed. Native
encryption unlock is integrated directly and does not require a hook. For
example, keep the root descriptor plaintext and configure repeatable setup:

```yaml
hooks:
  post-init:
    - ./scripts/ensure-local-setup
```

A failed hook returns a nonzero status and stops initialization. The checkout
remains intact. Retrying init on it runs post-init only, so recover failed
post-clone commands manually or put repeatable unlock/setup commands in post-init.
See [Lifecycle hooks](../configuration/root.md#lifecycle-hooks) for ordering, environment,
working directories, and failure semantics.
