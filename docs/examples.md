# Examples

These layouts progress from one overlay to composition and editable checkouts.
Each example identifies its root and overlay descriptors. Paths below assume
the source repository is `~/.dotfiles`, initialized with Git. Create the shown
content files as well as descriptors before running commands. If you are new to
Over, [Getting started](index.md#getting-started) first provides an isolated,
copyable walkthrough.

## One flat overlay

Use the repository root itself as an overlay when all files share one target:

```text
~/.dotfiles/
├── over.toml
├── .gitconfig
└── .config/
    └── starship.toml
```

Root descriptor, `~/.dotfiles/over.toml`:

```toml
target = "~"
```

Content, `~/.dotfiles/.gitconfig`:

```gitconfig
[init]
    defaultBranch = main
```

Content, `~/.dotfiles/.config/starship.toml`:

```toml
add_newline = false
```

```sh
over apply . --dry-run
over apply .
over status .
```

`.` explicitly selects the repository-root overlay. A bare `over apply` prompts
unless a default is configured, even when only one candidate exists. Existing
target files may conflict; inspect the preview before choosing how to handle
them. Editing an applied symlink edits its source in `~/.dotfiles`.

## Compose a host

Use one overlay per app and a host overlay to select them. This is an alternative
to the flat layout above; move content into the app directories when adopting it.

```text
~/.dotfiles/
├── over.toml
├── apps/
│   ├── git/
│   │   ├── over.toml
│   │   └── .gitconfig
│   └── starship/
│       ├── over.toml
│       └── .config/
│           └── starship.toml
└── hosts/
    └── laptop/
        └── over.toml
```

Root descriptor, `~/.dotfiles/over.toml`:

```toml
target = "~"
default_overlay = "hosts/laptop"
```

App descriptors, both `apps/git/over.toml` and `apps/starship/over.toml`:

```toml
# Inherit target from the repository root; no local fields needed.
```

Use the `.gitconfig` and `starship.toml` content from the flat example, now in
their app directories. Host descriptor, `hosts/laptop/over.toml`:

```toml
description = "Laptop dotfiles"
uses = ["apps/git", "apps/starship"]

[data]
theme = "dark"
```

```sh
over apply hosts/laptop --dry-run
over apply hosts/laptop
over status
over status --all
```

The host includes each app dependency once. Each app retains its own target;
`uses` is a union of independent trees, not a file override stack. Shared
dependencies in larger compositions are not repeated. Names in `uses` are
explicit identifiers, not globs.

The fixed default makes `over apply` select this host and `over status` report
its composition. For multiple hosts, add a descriptor for each, then use
`default_overlay = "hosts/{{ machine.hostname }}"` only after creating a matching
overlay for every machine that will use the repository. A missing default is an
error, including when selecting another overlay explicitly.

## Inherit shared policy

Use an intermediate descriptor to set policy for a family of overlays. Add this
family to the composed repository:

```text
~/.dotfiles/workspaces/
├── over.toml
└── demo/
    ├── over.toml
    └── notes.txt
```

Intermediate descriptor, `workspaces/over.toml`:

```toml
target = "~/Workspaces/{{ overlay.name | split('/') | last }}"
mode = "symlink"
```

Overlay descriptor, `workspaces/demo/over.toml`:

```toml
description = "Demo workspace"
```

Content, `workspaces/demo/notes.txt`:

```text
Notes for the demo workspace.
```

```sh
over apply workspaces/demo --dry-run
over apply workspaces/demo
```

The resolved destination is `~/Workspaces/demo/notes.txt`. A nearer explicit
target would replace the inherited template. Intermediate descriptors supply
configuration; discovery lists the more-specific overlay rather than listing
both parent and child. Ancestor settings follow field-specific
[inheritance rules](configuration/overlay.md#fields-and-inheritance).

## One workspace, several destinations

Use directory mappings to keep related files together while deploying them to
several writable locations. This example can coexist with the preceding ones.

```text
~/.dotfiles/workspace/client/
├── over.yaml
├── main/
│   └── notes.txt
└── config/
    └── settings.json
```

Overlay descriptor, `workspace/client/over.yaml`:

```yaml
target: ~/Workspace/Client
map:
  config:
    target: ~/.config/client
```

Content, `workspace/client/main/notes.txt`:

```text
Client workspace notes.
```

Content, `workspace/client/config/settings.json`:

```json
{"theme": "dark"}
```

```sh
over apply workspace/client --dry-run
over apply workspace/client
```

`main/notes.txt` lands at `~/Workspace/Client/main/notes.txt`, while
`config/settings.json` lands directly at `~/.config/client/settings.json`.
There is no `~/Workspace/Client/config` directory. If you later create
`~/.config/client/other.json`, `over add --overlay workspace/client
~/.config/client/other.json` reverse-routes it to `config/other.json`.

See [Directory mappings](configuration/overlay.md#directory-target-mappings) for
overlap restrictions and ownership boundaries.

## Edit ordinary files with checkout

Choose checkout when applications should edit ordinary target files rather than
the linked source. Starting from the workspace mapping example, add this field
to its overlay descriptor **before the first apply**:

```yaml
# workspace/client/over.yaml (complete descriptor)
target: ~/Workspace/Client
mode: checkout
map:
  config:
    target: ~/.config/client
```

Commit the descriptor and both content files in the source repository first;
initial checkout materialization reads committed `HEAD`. With Git author identity
configured, run:

```sh
git -C ~/.dotfiles add workspace/client
git -C ~/.dotfiles commit -m "Add client workspace"
over apply workspace/client --dry-run
over apply workspace/client
```

If you already applied the symlink example, unapply it using the old descriptor
before switching this example to checkout. Each mapped destination is a separate
checkout unit, with no target `.git` directory.

After editing `~/.config/client/settings.json`:

```sh
over diff workspace/client
over add --overlay workspace/client ~/.config/client/settings.json
over diff workspace/client --staged
over commit workspace/client -m "Update client settings"
over sync workspace/client --dry-run
```

Add stages a target snapshot; commit records **only staged changes** in the source
repository. Further unstaged target edits stay outside that commit. Sync reconciles
source and target locally, without fetching or pushing a remote.

For whole-directory links instead, use `mode: symlink-directory` on directories
that contain no control metadata or ignored content. See
[Materialization](configuration/materialization.md) for all mechanisms, ordered
rules, and safe migration of existing ownership boundaries.
