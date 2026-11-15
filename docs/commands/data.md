# `over data`

Export the same stable data context used by templates and hooks as JSON.

```text
Usage: over data [OPTIONS] [NAME]

Arguments:
  [NAME]  Select an overlay (defaults to the configured host)

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Selection and Composition

Without `NAME`, select the configured `default_overlay` (the host) first, including
machine-based selection such as `hosts/{{ machine.hostname }}`. Without a configured
host, use [contextual selection](../usage/selection.md), failing if no overlay can
be selected. An explicit name selects that overlay. The command never prompts or
implicitly selects all overlays.

The selected overlay determines `data` and the transitive `uses` namespace.
The configured host supplies only its own host overrides; its unrelated dependencies
are not traversed or included. A missing or malformed configured host remains an error
even with an explicit selection.

## Output and Scripting

Every successful invocation emits the same three-part envelope:

```json
{
  "data": {},
  "overlays": {},
  "host": {}
}
```

- `data`: the selected overlay's own declarations, with no inheritance or merging.
- `overlays`: only its transitive `uses` dependencies, with canonical paths presented
  as nested mappings: `apps/ssh` becomes `overlays.apps.ssh`. Terminal nodes expose
  `name`, `root`, `target`, and their own declared `data` and `host` mappings.
- `host`: a recursive merge of selected-local host defaults and the configured
  host's own overrides, with host values winning. Without a host, only local defaults
  apply. Maps merge; lists, scalars, and nested nulls replace, never concatenate.

See [data resolution](../configuration/overlay.md#resolution-and-precedence) for
examples, namespace collisions, bracket indexing, and migration guidance. Dependency
target attributes are resolved just as for templates; invalid target templates fail.

Stdout contains only JSON followed by a newline. Terminals receive pretty-printed
JSON; pipes receive compact, uncolored JSON. Values retain their JSON types.
Diagnostics go to stderr. Selection, resolution, or serialization errors return a
nonzero exit status without a JSON payload.

```sh
# Inspect the configured host, or contextual overlay when no host is configured
over data

# Inspect one overlay's stable scope
over --home ~/.dotfiles data apps/ssh

# Query each namespace explicitly
over data apps/ssh | jq '.data.archlinux.packages'
over data | jq '.overlays.apps.ssh.data.archlinux.packages'
over data apps/ssh | jq -r '.host.ssh.agent'
```

Data strings remain literal. Only target/path definitions are rendered; metadata
values are not recursively templated. Filtering and package aggregation belong to
the consuming tools, not the generic data resolver.
