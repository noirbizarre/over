# ADR-003: Overlay descriptors tolerate any format on read; `format` only governs what `over new` writes

**Status:** accepted

## Context

`over` accepts overlay descriptors in TOML or YAML. Two options existed for
how strict to be about which files are actually read: make the root config's
`format` preference authoritative (only read the preferred extension, ignore
the others), or accept whichever extension is present regardless of
preference — easing migration and mixed authoring within one repository.

## Decision

Reading never distinguishes formats: `Overlay::new` builds its config
sources via `config::File::with_name(basename)` with no extension, and the
`config` crate probes every registered extension (`over.toml`, `over.yaml`,
`over.yml`) itself. `Format` (`src/overlays/mod.rs`) and `Repository::
preferred_format()` only decide which extension `over new` **writes** — see
[Configuration](../configuration.md#format-preference).

## Consequences

An overlay author can mix formats freely across a tree, and migrating one
overlay from TOML to YAML costs nothing (both are read the same way while
the old file is deleted). But if both `over.toml` and `over.yaml` exist in
the same directory, `config` merges them silently rather than reporting a
conflict — which is exactly the footgun `over lint`'s
`check_multiple_descriptors` check exists to catch, because the read path
itself does not. The `format` preference is also easy to misread as
governing what gets loaded; it only governs what gets written.
