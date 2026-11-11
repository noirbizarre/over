# `over data`

Export resolved overlay data as JSON. This is the supported CLI interface for
external consumers of the custom data context introduced in
[#179](https://github.com/noirbizarre/over/issues/179).

```text
Usage: over data [OPTIONS] [NAME]

Arguments:
  [NAME]  Name of the overlay to compose with the configured default context

Options:
  -H, --home <HOME>  Configuration and overlays root [env: OVER_HOME]
  -d, --debug        Toggle debug traces
  -v, --verbose      Toggle verbose output
  -h, --help         Print help
```

## Selection and Composition

Without `NAME`, [infer from context](selection.md), then use the configured
`default_overlay`, including machine-based templates such as
`{{ machine.hostname }}`. If neither selects an overlay, fail with an actionable
error. Ambiguity requires an explicit name, even with a default. The selected
overlay includes its transitive `uses` dependencies. The command never prompts
or implicitly aggregates all overlays.

With `NAME`, the command composes the default context (when configured) with the
named overlay and both transitive dependency closures. Without a default, only
the named overlay and its dependencies contribute. An invalid configured default
is an error even when an explicit name is supplied.

The command uses the same [data resolution rules](../configuration/overlay.md#resolution-and-precedence)
as templates and the public Rust API: dependencies precede their parents,
shared dependencies contribute once, and the selected overlay's effective
inherited mapping merges last. Mappings merge recursively; lists and other
values replace earlier values, including explicit nulls.

## Output and Scripting

Successful stdout contains only the resolved data mapping as JSON, followed by a
newline. Terminals receive pretty-printed JSON using the default foreground.
Piped or redirected stdout receives compact, uncolored JSON. There is no wrapper object or
execution-context metadata.
Booleans, numbers, strings, arrays, objects, and explicit null values retain their
JSON types. A selected overlay with no data outputs `{}` successfully.

Diagnostics, including debug traces, go to stderr. Missing or unknown selections,
invalid defaults or descriptors, missing dependencies, dependency cycles, and
serialization failures produce a nonzero exit status. Selection, resolution,
and serialization failures do not write a JSON payload to stdout.

```sh
# Dump the contextual overlay data, falling back to default_overlay
over data

# Compose the host context with another overlay
over data base

# Select a different repository
over --home ~/.over data base

# Query values using standard JSON tooling
over data | jq '.provisioning.archlinux'
over data | jq -r '.hostname'

# Pretty-print for inspection
over data | jq .
```

Data strings remain literal values; the command does not render them as templates
or interpret consumer-specific namespaces. Use external tools for querying and
filtering the JSON output.
