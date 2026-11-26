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

With `--install`, root `hooks.install` runs once after dependency/data/target
resolution and before any overlay deployment. After all participating overlays
finish reconciliation, including Git provisioning and Git-exclude updates,
overlays' own install hooks run in dependency-first `uses` order. Then root and
overlay post-apply hooks run in their existing order. Finally, root-only
`hooks.post-install` runs once, after every earlier phase succeeds.

Overlay install commands can use configuration deployed by the same invocation,
and post-apply hooks can use installed applications. Root install commands run
before that configuration exists; use root post-install for invocation-wide
finalization. Neither root install phase runs without `--install`.

Install hooks use the same [structured-hook features](configuration/root.md#lifecycle-hooks)
as other events: shorthand or templated `command`, `when`, `cwd`, and `env`.
Conditions run before execution fields render; a skipped hook neither resolves
execution-only secrets nor spawns a process. `machine.os` is `linux`, `macos`,
or `windows`; select Linux distributions with `machine.distro_id`.

`paths.home` is the actual user's home, `paths.over` the source repository root,
and `paths.target` the current overlay's effective destination. Use
`cwd: "{{ paths.target }}"` for work in deployed configuration. Root hooks see
the selected overlay only when one distinct root is selected. With multiple
distinct roots, root hooks have no `overlay`, `paths.target`, or `OVER_OVERLAY_*`
variables; `OVER_TARGET_ROOT` remains available. Guard optional target access
with `paths.target is defined`. Each overlay hook sees its own target and the
local `data`, effective `host`, and explicit dependency `overlays`. Default cwd is the declaring overlay's source directory,
or the repository root for root hooks. Explicit relative cwd is repository-root-relative.

Hooks do not inherit through ancestor descriptors. Put shared steps in root
`hooks.install`, or compose an overlay that owns them through `uses`.
`--no-uses` includes only root and selected-overlay hooks. A diamond dependency
participates once; identical commands in different overlays still both run.
Initialization and inspection commands never run install hooks.
`over apply --read-only --install` still runs the requested hooks.

## Explicit package aggregation

An overlay's `data` contains its own complete package list, without automatic
aggregation. Hooks may explicitly read dependency lists through `overlays`.
For example, a hook consuming its own list:

```yaml
# apps/ssh/over.yaml
data:
  archlinux:
    packages: [openssh]
hooks:
  install:
    - command: "pacman -S --needed {{ data.archlinux.packages | join(' ') }}"
      sudo: true
      when: machine.distro_id == "arch"
```

Root hooks can inspect all actual participants through `allowOverlays`, an ordered
sequence of overlay objects ready before root install. It includes selected roots
and transitive dependencies once each in deterministic application order. Each
object exposes its own local `data`; no lists are automatically merged. With
`--no-uses`, only selected overlays appear. For example, users can pass overlay
metadata to their own script in one invocation:

```yaml
# Repository-root over.yaml
hooks:
  install:
    - command: ./scripts/prepare-overlays
      env:
        OVERLAY_ROLES: "{% for item in allowOverlays %}{{ item.data.role | default(item.name) }} {% endfor %}"
  post-install:
    - ./scripts/finalize-install
```

For host provisioning, select the host overlay whose `uses` lists all desired
overlays. A provisioning script can collect local packages from that host and
every dependency, add the effective host-specific list, and deduplicate explicitly:

```sh
over data | jq -r '
  def overlay_packages:
    if (.name? | type) == "string" then
      (.data.archlinux.packages // []),
      (del(.name, .root, .target, .data, .host)[] | overlay_packages)
    else .[] | overlay_packages end;
  [(.data.archlinux.packages // [])[],
   (.host.archlinux.packages // [])[],
   (.overlays | overlay_packages)[]]
  | unique | .[]
'
```

This prints a deduplicated package list for the provisioning layer to install.
Neither `data` nor the host deep merge aggregates package lists automatically.
See [data scopes and migration](configuration/overlay.md#resolution-and-precedence).

## Failures, retries, and trust

The first failing hook stops later hooks and fails apply. Root install failure
prevents overlay deployment. Deployment or overlay hook failure prevents root
post-install; post-install failure fails the overall command.
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
Descriptors using them warn with migration guidance and ignore those declarations,
including inherited ancestor descriptors. Replace them with explicit
`hooks.install` commands or scripts:

```yaml
# Replace an old install.apt package list with an explicit command.
hooks:
  install:
    - command: apt-get install -y neovim
      sudo: true
      when: machine.os == "linux" and machine.distro_id == "ubuntu"
```

Move per-overlay pre/post-install steps into the same ordered command list, or
use root install/post-install hooks for invocation-wide setup/finalization. Choose manager
flags, platform conditions, and cwd explicitly; Over no longer chooses a package
manager or merges package lists. Move formerly inherited steps to root hooks or
an explicitly composed overlay, since hook lists do not inherit.

Overlay installation follows deployment; root installation precedes it. Review
scripts that relied on the old order. See [ADR-027](adr/027-installation-is-an-opt-in-hook-event.md)
for the architecture decision replacing the dedicated engine.
