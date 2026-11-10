# Install hooks

Describe installation steps as ordinary lifecycle hooks, in the repository-root
descriptor or an overlay's own descriptor:

```yaml
hooks:
  install:
    - command: paru -S --needed neovim
      when: machine.os == "linux" and machine.distro_id == "arch"
    - command: brew install neovim
      when: machine.os == "macos"
    - command: mise install
      cwd: "{{ paths.home }}"
      env:
        EDITOR_HOST: "{{ machine.hostname }}"
```

Run them explicitly:

```sh
over apply hosts/laptop --install
```

Ordinary `over apply` never runs or prepares install commands. `-i` is an alias
for `--install`; dry runs execute no hooks, even with the flag.

## Lifecycle and context

All participating overlays must finish reconciliation, including Git provisioning
and Git-exclude updates, before any install hook runs. With `--install`, root
install hooks run first, then participating overlays' own install hooks in
dependency-first `uses` order. All install hooks finish before post-apply hooks
start. Commands can therefore use configuration deployed by the same invocation,
and post-apply hooks can use installed applications.

Install hooks use the same [structured-hook features](configuration/root.md#lifecycle-hooks)
as other events: shorthand or templated `command`, `when`, `cwd`, and `env`.
Conditions run before execution fields render; a skipped hook neither resolves
execution-only secrets nor spawns a process. `machine.os` is `linux`, `macos`,
or `windows`; select Linux distributions with `machine.distro_id`.

`paths.home` is the actual user's home, `paths.over` the source repository root,
and `paths.target` the current overlay's effective destination. Use
`cwd: "{{ paths.target }}"` for work in deployed configuration. Root hooks see
the selected overlay; each overlay hook sees its own target and the selected
consumer's resolved data. Default cwd is the declaring overlay's source directory,
or the repository root for root hooks. Explicit relative cwd is repository-root-relative.

Hooks do not inherit through ancestor descriptors. Put shared steps in root
`hooks.install`, or compose an overlay that owns them through `uses`.
`--no-uses` includes only root and selected-overlay hooks. A diamond dependency
participates once; identical commands in different overlays still both run.
Initialization and read-only commands never run install hooks.

## Failures, retries, and trust

The first failing hook stops later hooks, including post-apply, and fails apply.
Diagnostics identify the event, scope, and command index without printing command
or rendered values. Completed file changes and command effects remain. Numeric
child exit status is preserved where representable; other failures return `1`.
Nested Over commands suppress further lifecycle dispatch.

Repeated `over apply --install`, including no-change applies and retries, may
execute every install command again. Over maintains no package identities,
package deduplication, or persistent installation state. Commands and scripts are
responsible for their own idempotency and prompt policies. Over's force/no-prompt
flags do not control child prompts, and child output is not redacted.

**Install hooks are arbitrary executable commands. Applying an untrusted
repository with `--install` executes repository-provided code.** Review commands
and scripts before running them. See [Safety and recovery](usage/safety.md).

## Migrate legacy installation configuration

The top-level `install` key and its package-manager/platform tables are removed.
Descriptors using them fail with migration guidance, including inherited ancestor
descriptors. Replace them with explicit `hooks.install` commands or scripts:

```yaml
# Replace an old install.apt package list with an explicit command.
hooks:
  install:
    - command: sudo apt-get install -y neovim
      when: machine.os == "linux" and machine.distro_id == "ubuntu"
```

Move pre/post-install steps into the same ordered command list. Choose manager
flags, platform conditions, and cwd explicitly; Over no longer chooses a package
manager or merges package lists. Move formerly inherited steps to root hooks or
an explicitly composed overlay, since hook lists do not inherit.

Installation now follows deployment rather than preceding it. Review scripts
that relied on the old order. See [ADR-027](adr/027-installation-is-an-opt-in-hook-event.md)
for the architecture decision replacing the dedicated engine.
