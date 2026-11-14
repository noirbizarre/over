# Consistency Report

**Date:** 2026-11-14

**Project:** over

**Scope:** Doc-to-Doc, Doc-to-Code, Code-to-Code

## Summary

| Severity | Count |
|---|---:|
| Critical | 1 |
| Major | 10 |
| Minor | 1 |
| **Total** | **12** |

Duplicate findings across review types are combined below. Findings are based on static source tracing; no runtime reproductions, builds, or tests were run. Generated files and explicitly superseded historical behavior were excluded.

## Critical Issues

### C1 — [Doc-to-Code / Code-to-Code] Generated checkouts bypass ordinary filesystem protections

**Files:**

- `docs/usage/safety.md:93–108` promises inode and native-metadata preservation for virtual refreshes.
- `src/materialize/virtual_checkout/git.rs:298–325` rejects ordinary updates to read-only files.
- `src/actions/fs.rs:97–240` implements metadata-preserving replacement.
- `src/materialize/virtual_checkout/generated.rs:713–744` instead uses a temporary-file replacement, restoring only permission bits.
- `src/materialize/rendered.rs:49–65` creates and publishes the new inode.

**Description:** A checkout containing a templated file uses the generated reconciliation backend for ordinary siblings too. That backend bypasses the ordinary read-only preflight and metadata-preserving writer. Existing ownership, ACLs, and xattrs are not copied. Executable-bit changes also differ: an existing private `0600` file can become `0711` rather than `0700`.

**Impact:** Adding a templated sibling can change the safety guarantees of otherwise ordinary files. Sync can replace read-only files, lose native access metadata, and widen executable access contrary to the documented preservation contract.

**Suggested fix:** Reuse ordinary preflight and metadata-preserving update policies in generated checkouts, preserving explicit generated-file permission assignments. Add paired backend regressions for read-only files, private executable transitions, native metadata, and hard links.

## Major Issues

### M1 — [Doc-to-Doc / Doc-to-Code] Rule documentation omits valid rendering-only assignments

**Files:**

- `docs/usage/lint.md:164–170`
- `docs/configuration/overlay.md:252–253`
- `docs/configuration/reference.md:157–158`
- `src/overlays/rules.rs:68–76`

**Description:** The lint and overlay guides say a rule must assign `target`, `mode`, or `permissions`. The reference and validator also accept `template` or `strip_extension` alone.

**Impact:** Valid rendering-only rules are described as invalid, encouraging unnecessary assignments that can override inherited policy.

**Suggested fix:** List all five supported non-null assignments consistently, including that explicit `false` is a valid assignment.

### M2 — [Doc-to-Doc / Doc-to-Code] Permission documentation excludes generated outputs

**Files:**

- `docs/configuration/overlay.md:376–384`
- `docs/configuration/reference.md:39,146–152`
- `docs/faq.md:223–229`
- `src/desired/entry.rs:231–237`
- `src/overlays/overlay.rs:317–320`
- `src/desired/content.rs:181–187,239–250`
- `src/materialize/virtual_checkout/generated.rs:370–380,723–740`

**Description:** Several guides and API comments restrict permission policy to partial/decrypted files or exclude virtual-checkout content. Implementation also enforces configured permissions on generated artifacts and generated physical checkout files. Another reference section already describes generated-output support.

**Impact:** Users can misunderstand which inherited permission rules affect generated content, or assume those rules cannot repair its access modes.

**Suggested fix:** Align guides and API comments around ordinary source links, ordinary checkout files, generated outputs, and partial/decrypted outputs; document generated defaults and explicit permission enforcement.

### M3 — [Doc-to-Doc / Doc-to-Code] Shared hook reference overpromises selected-overlay context

**Files:**

- `docs/configuration/root.md:235–238,342–344`
- `docs/configuration/overlay.md:103–104`
- `docs/install-config.md:45–48`
- `docs/usage/apply.md:94–98`
- `src/overlays/overlay.rs:454–499`
- `src/hooks.rs:444–460`

**Description:** Shared references say repository hooks receive the selected overlay. With multiple distinct selected roots, implementation deliberately supplies no `overlay` context and removes all `OVER_OVERLAY_*` variables. The apply guide correctly documents that exception.

**Impact:** Hooks written against the shared reference can fail during multi-root apply after materialization has completed.

**Suggested fix:** Qualify the guarantee as single-distinct-root behavior. Document that multi-root repository hooks retain `OVER_TARGET_ROOT`, while participant hooks retain their own overlay identity.

### M4 — [Doc-to-Doc / Doc-to-Code] Restore reference omits generated-checkout semantics

**Files:**

- `docs/usage/restore.md:26–47,131–134`
- `docs/usage/sync.md:137–140`
- `src/restore/mod.rs:108–169`
- `src/materialize/virtual_checkout/generated.rs:863–937`

**Description:** The restore reference describes the index as the default source without a generated-file exception. Generated destinations instead restore the recorded generated snapshot, clear associated conflict bookkeeping, and reject `--staged` and `--source`. Ordinary siblings retain normal restore semantics.

**Impact:** Users choosing a destructive recovery operation are given the wrong account of what content will be restored and which flags are supported.

**Suggested fix:** Add a generated-checkout subsection covering the recorded snapshot source, supported flags, conflict clearing, interrupted-publication refusal, and ordinary-sibling distinction.

### M5 — [Doc-to-Code] Rendered-file conflicts do not follow documented interactive handling

**Files:**

- `docs/usage/apply.md:45–51`
- `docs/usage/safety.md:10–17`
- `src/materialize/rendered.rs:95–119`

**Description:** Apply documentation describes interactive conflict choices and force-authorized directory replacement. Rendered symlink-mode files have no conflict menu: unmanaged destinations fail unless forced, and directory obstructions fail even with force.

**Impact:** Users following the documented conflict workflow encounter an error rather than the promised choices; retrying with force still cannot resolve directories.

**Suggested fix:** Document the rendered-file exception: no interactive menu or Absorb, force for eligible non-directory replacements, and manual handling of directory obstructions.

### M6 — [Code-to-Code] Sync publishes new checkout leaves without updating host Git exclusions

**Files:**

- `src/overlays/overlay.rs:605–622` reconciles exclusions during apply.
- `src/git_exclude/mod.rs:228–267` excludes scoped checkout leaves individually.
- `src/sync/mod.rs:633–673,884–903` publishes generated and ordinary checkout updates.
- `src/cli/sync.rs:249–256` invokes sync without exclusion reconciliation.

**Description:** Source additions can create newly managed target files during sync, but exclusion maintenance remains tied to other workflows.

**Impact:** In a host Git worktree, newly deployed files can appear as untracked and become eligible for accidental staging until another apply repairs exclusions.

**Suggested fix:** Integrate exclusion maintenance with sync publication, including generated destination renames, dry-run handling, and failure behavior. Modify `src/sync/mod.rs`, `src/git_exclude/mod.rs`, and affected tests.

### M7 — [Code-to-Code] Exclusions forget historical leaves that checkout ownership still retains

**Files:**

- `src/materialize/virtual_checkout/state.rs:437–487`
- `src/status/virtual_checkout.rs:151–176`
- `src/git_exclude/mod.rs:242–267`
- `src/materialize/virtual_checkout/mod.rs:115–124`

**Description:** Ownership and status retain recorded-base paths, but ordinary scoped-checkout exclusions use current source HEAD and index only. Reapply does not synchronize an existing checkout.

**Impact:** After a source deletion and before target sync, reapply can remove the exclusion for a still-existing, still-owned target file—even one with local changes.

**Suggested fix:** Derive exclusions from shared ownership inventory, including retained base/projection leaves, while respecting ownership holes. Modify exclusion inventory and ownership helpers as needed, with regression tests.

### M8 — [Code-to-Code] Normal exclusion reconciliation overwrites linked-worktree contributions

**Files:**

- `src/git_exclude/mod.rs:473–486,512–517`
- `src/git_exclude/mod.rs:646–670`
- `src/git_exclude/mod.rs:779–783`

**Description:** Normal reconciliation groups by each worktree’s Git directory, but those groups write the same common `info/exclude` file and overlay marker. Each rewrite can replace another worktree’s contribution. Repair already aggregates by the shared physical destination.

**Impact:** An overlay targeting different paths in linked host worktrees can leave only one worktree’s expected patterns in its managed block.

**Suggested fix:** Share common-destination aggregation between normal reconciliation and repair; check diagnosis and removal for the same identity mismatch. Modify `src/git_exclude/mod.rs` and add linked-worktree regressions.

### M9 — [Code-to-Code] Unapply calls an existing stale rendered link “already absent”

**Files:**

- `src/materialize/rendered.rs:79–90`
- `src/status/mod.rs:262–267`
- `src/unapply/mod.rs:235–240`
- `src/cli/unapply.rs:134–154`

**Description:** For rendered files, `Operation::Create` can mean an existing stale artifact link needs replacement. Unapply treats every such operation as absence, although status already accounts for this distinction.

**Impact:** After template/context changes, unapply can report success, retain the deployed link, and remove its host exclusion.

**Suggested fix:** Classify rendered unapply using actual existence and verifiable ownership. Remove only provably owned links; otherwise report retention requiring attention rather than absence. Modify unapply classification and rendered ownership helpers as needed, with regressions.

### M10 — [Code-to-Code] Generated sync preview suppresses known conflict status

**Files:**

- `src/materialize/virtual_checkout/generated.rs:490–526,780–798`
- `src/sync/mod.rs:115–119,641–673`

**Description:** Dry-run detects and prints generated conflicts, then returns success. Real execution reports those conflicts as blocked. Consequently, the wrapper emits a non-attention `Preview` for a known conflict.

**Impact:** Automation cannot reliably use preview outcomes or exit status to distinguish a viable sync from one already known to require intervention.

**Suggested fix:** Preserve structured conflict information through preview and execution so both have matching attention semantics without preview mutations. Modify generated reconciliation and sync outcome handling, with regressions.

## Minor Issues

### m1 — [Doc-to-Doc] Inline documentation conflates declared-repository sync with CLI sync

**Files:**

- `src/materialize/checkout.rs:3–8`
- `src/status/git.rs:7–16`
- `src/sync/mod.rs:1–5`
- `docs/usage/sync.md:92–97`

**Description:** Materializer/status comments direct declared-repository lifecycle synchronization to `over sync`. The current CLI excludes those repositories; the broader library API retains separate functionality and already documents that distinction.

**Suggested fix:** Correct the materializer/status comments to direct users to ordinary Git for declared repositories. Link retained synchronization capabilities explicitly to the library API rather than the CLI.

## Potential Issues

None retained. Findings above have supporting implementation or documentation evidence, though runtime regressions still need verification.

## Coverage and limitations

Reviewed authored Markdown documentation, current configuration and command references, relevant ADRs, manifests, API comments, CLI wiring, desired-state construction, materialization, status, sync, restore, unapply, host exclusions, and adjacent regression coverage.

This was a broad static audit, not a proof of exhaustive correctness. Platform-specific behavior and proposed reproduction scenarios were not executed. No application, configuration, or documentation files were changed during the review.

*Report generated using the consistency skill.*
