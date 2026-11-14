//! Conservative textual translation: rendered output is never treated as source.

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use similar::TextDiff;

use crate::desired::{DesiredEntry, Provenance};
use crate::exec::Context;
use crate::materialize::disposal::{PathGuard, validate_relative_ancestors};
use crate::materialize::virtual_checkout::{git, state};
use crate::overlays::Overlay;

use super::CommitOptions;

#[derive(Clone, Debug)]
struct Edit {
    range: Range<usize>,
    replacement: Vec<String>,
}

fn lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

fn edits(before: &str, after: &str) -> Vec<Edit> {
    let diff = TextDiff::from_lines(before, after);
    let after = lines(after);
    diff.ops()
        .iter()
        .filter(|op| op.tag() != similar::DiffTag::Equal)
        .map(|op| Edit {
            range: op.old_range(),
            replacement: after[op.new_range()].to_vec(),
        })
        .collect()
}

fn apply(before: &[String], changes: &[Edit]) -> String {
    let mut output = before.to_vec();
    for edit in changes.iter().rev() {
        output.splice(edit.range.clone(), edit.replacement.clone());
    }
    output.concat()
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    // Insertions on a changed boundary have no defensible ordering.
    if left.is_empty() || right.is_empty() {
        left.start <= right.end && right.start <= left.end
    } else {
        left.start < right.end && right.start < left.end
    }
}

fn merge(base: &str, current: &str, translated: &[Edit]) -> Result<String> {
    let mut independent = edits(base, current);
    ensure!(
        !independent.iter().any(|other| translated
            .iter()
            .any(|edit| overlaps(&edit.range, &other.range))),
        "source changes overlap a rendered hunk"
    );
    independent.extend_from_slice(translated);
    independent.sort_by_key(|edit| edit.range.start);
    Ok(apply(&lines(base), &independent))
}

fn matches(haystack: &[String], needle: &[String]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, candidate)| (candidate == needle).then_some(index))
        .collect()
}

fn validate_expressions(source: &str) -> Result<()> {
    let mut rest = source;
    while let Some(start) = rest.find("{{") {
        let bytes = rest.as_bytes();
        let mut cursor = start + 2;
        let mut quote = None;
        loop {
            ensure!(cursor + 1 < bytes.len(), "unterminated template expression");
            ensure!(
                bytes[cursor] != b'\n' && bytes[cursor] != b'\r',
                "multiline template expressions are unsupported"
            );
            // Quoted delimiters are expression data, not the end of a Jinja region.
            if quote.is_some() && bytes[cursor] == b'\\' {
                ensure!(
                    !matches!(bytes[cursor + 1], b'\n' | b'\r'),
                    "multiline template expressions are unsupported"
                );
                cursor += 2;
                continue;
            }
            if quote == Some(bytes[cursor]) {
                quote = None;
            } else if quote.is_none() {
                if matches!(bytes[cursor], b'\'' | b'"') {
                    quote = Some(bytes[cursor]);
                } else if &bytes[cursor..cursor + 2] == b"}}" {
                    rest = &rest[cursor + 2..];
                    break;
                }
            }
            cursor += 1;
        }
    }
    Ok(())
}

/// Only whole literal lines are eligible; expressions may emit arbitrary lines.
fn translate(source: &str, rendered: &str, local: &str, template: bool) -> Result<Vec<Edit>> {
    let source_lines = lines(source);
    let rendered_lines = lines(rendered);
    let changes = edits(rendered, local);
    if !template {
        ensure!(
            source == rendered,
            "recorded source does not match rendered base"
        );
        return Ok(changes);
    }
    // No source maps: block structure and trimming can invalidate literal provenance.
    ensure!(
        !["{%", "{#", "{{-", "-}}"]
            .iter()
            .any(|marker| source.contains(marker)),
        "template control structures, comments, or whitespace trimming are unsupported"
    );
    validate_expressions(source)?;
    let literal = |slice: &[String]| {
        slice
            .iter()
            .all(|line| !line.contains("{{") && !line.contains("}}"))
    };
    let mut translated = Vec::new();
    for (number, change) in changes.into_iter().enumerate() {
        let result = (|| -> Result<Edit> {
            ensure!(
                !change
                    .replacement
                    .iter()
                    .any(|line| ["{{", "{%", "{#", "}}", "%}", "#}"]
                        .iter()
                        .any(|marker| line.contains(marker))),
                "new content contains template syntax"
            );
            let (context, offset) = if change.range.is_empty() {
                // Insertions need literal neighbors, including a unique end-of-file anchor.
                let start = change.range.start.saturating_sub(1);
                let end = (change.range.end + 1).min(rendered_lines.len());
                (start..end, change.range.start - start)
            } else {
                (change.range.clone(), 0)
            };
            let needle = &rendered_lines[context.clone()];
            ensure!(
                matches(&rendered_lines, needle).len() == 1,
                "ambiguous rendered hunk anchors"
            );
            let found = matches(&source_lines, needle);
            ensure!(
                found.len() == 1,
                "hunk does not uniquely match literal source content"
            );
            let start = found[0];
            ensure!(
                literal(&source_lines[start..start + needle.len()]),
                "hunk overlaps a Jinja expression"
            );
            if change.range.is_empty() {
                ensure!(
                    context.start != 0 || start == 0,
                    "ambiguous beginning-of-file insertion"
                );
                ensure!(
                    context.end != rendered_lines.len()
                        || start + needle.len() == source_lines.len(),
                    "ambiguous end-of-file insertion"
                );
            }
            let start = start + offset;
            Ok(Edit {
                range: start..start + change.range.len(),
                replacement: change.replacement,
            })
        })();
        translated.push(result.with_context(|| {
            format!(
                "rendered hunk {} (old lines {}..{})",
                number + 1,
                change.range.start + 1,
                change.range.end + 1
            )
        })?);
    }
    translated.sort_by_key(|edit| edit.range.start);
    ensure!(
        !translated
            .windows(2)
            .any(|pair| overlaps(&pair[0].range, &pair[1].range)),
        "translated hunks overlap"
    );
    Ok(translated)
}

pub(crate) struct File {
    pub target: PathBuf,
    pub destination: PathBuf,
    pub source: PathBuf,
    pub path: PathBuf,
    pub mode: u32,
    pub indexed: Vec<u8>,
    pub worktree: Vec<u8>,
    pub rendered: Vec<u8>,
    pub rendered_mode: i32,
    pub expected: (git2::Oid, i32),
    pub local: Vec<u8>,
}

struct Check {
    path: PathBuf,
    bytes: Vec<u8>,
    guard: PathGuard,
    permissions: std::fs::Permissions,
}

#[derive(Default)]
pub(crate) struct Prepared {
    pub files: Vec<File>,
    pub conflicts: Vec<String>,
    pub selected_sources: std::collections::BTreeSet<PathBuf>,
    checks: Vec<Check>,
    records: Vec<(DesiredEntry, state::VirtualCheckoutRecord)>,
}

impl Prepared {
    pub fn journal(&self, repo: &git2::Repository, source_commit: git2::Oid) -> Result<()> {
        use crate::materialize::virtual_checkout::generated::{self, ReversePublication};
        for (entry, original) in &self.records {
            let files: Vec<_> = self
                .files
                .iter()
                .filter(|file| file.target == entry.target)
                .collect();
            if files.is_empty() {
                continue;
            }
            let mut record = original.clone();
            let generated = record
                .generated
                .as_mut()
                .context("missing generated snapshot")?;
            let parent = repo.find_commit(git2::Oid::from_str(&generated.base)?)?;
            let mut builder = git2::build::TreeUpdateBuilder::new();
            for file in &files {
                builder.upsert(
                    &file.destination,
                    repo.blob(&file.rendered)?,
                    git::filemode_from_raw(file.rendered_mode)
                        .context("unsupported rendered mode")?,
                );
            }
            let tree = repo.find_tree(builder.create_updated(repo, &parent.tree()?)?)?;
            let signature = git2::Signature::now("Over", "over@localhost")?;
            let mut observed_builder = git2::build::TreeUpdateBuilder::new();
            for file in &files {
                observed_builder.upsert(
                    &file.destination,
                    repo.blob(&file.local)?,
                    git::filemode_from_raw(file.rendered_mode)
                        .context("unsupported rendered mode")?,
                );
            }
            let observed_tree =
                repo.find_tree(observed_builder.create_updated(repo, &parent.tree()?)?)?;
            let observed_commit = repo.commit(
                None,
                &signature,
                &signature,
                "Reverse-patch consumed rendering",
                &observed_tree,
                &[&parent],
            )?;
            let observed_parent = repo.find_commit(observed_commit)?;
            let generated_commit = repo.commit(
                None,
                &signature,
                &signature,
                "Reverse-patched generated checkout",
                &tree,
                &[&parent, &observed_parent],
            )?;
            generated.reverse_pending = Some(ReversePublication {
                parent: repo.head()?.peel_to_commit()?.id().to_string(),
                source_commit: source_commit.to_string(),
                generated_commit: generated_commit.to_string(),
                destinations: files.iter().map(|file| file.destination.clone()).collect(),
                observed: files
                    .iter()
                    .filter(|file| file.local != file.rendered)
                    .map(|file| (file.destination.clone(), observed_commit.to_string()))
                    .collect(),
                index_before: git2::Oid::hash_object(
                    git2::ObjectType::Blob,
                    &std::fs::read(repo.path().join("index"))?,
                )?
                .to_string(),
                sources_before: files
                    .iter()
                    .map(|file| {
                        let before = self
                            .checks
                            .iter()
                            .find(|check| check.path == file.source)
                            .context("missing source snapshot")?;
                        Ok((
                            file.source.clone(),
                            git2::Oid::hash_object(git2::ObjectType::Blob, &before.bytes)?
                                .to_string(),
                        ))
                    })
                    .collect::<Result<_>>()?,
            });
            generated::persist(entry, &record)?;
        }
        Ok(())
    }

    fn capture(&mut self, path: &Path) -> Result<Vec<u8>> {
        let guard = PathGuard::capture(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(metadata.is_file(), "reverse patches require regular files");
        let bytes = std::fs::read(path)?;
        self.checks.push(Check {
            path: path.to_path_buf(),
            bytes: bytes.clone(),
            guard,
            permissions: metadata.permissions(),
        });
        Ok(bytes)
    }

    pub fn verify(&self) -> Result<()> {
        for (entry, record) in &self.records {
            ensure!(
                state::record_for_entry(entry)?.as_ref() == Some(record),
                "generated checkout state changed while preparing the commit"
            );
        }
        self.verify_inputs()
    }

    pub fn verify_inputs(&self) -> Result<()> {
        for check in &self.checks {
            check.guard.verify()?;
            let metadata = std::fs::symlink_metadata(&check.path)?;
            ensure!(
                metadata.is_file() && metadata.permissions() == check.permissions,
                "reverse-patch input '{}' changed kind or permissions; retry",
                check.path.display()
            );
            ensure!(
                std::fs::read(&check.path)? == check.bytes,
                "reverse-patch input '{}' changed while preparing the commit; retry",
                check.path.display()
            );
        }
        for (entry, record) in &self.records {
            state::verify(entry, record)?;
            let Provenance::Overlay { source, .. } = &entry.provenance else {
                unreachable!()
            };
            let (repo, _) = git::discover_source(source)?;
            let generated = record
                .generated
                .as_ref()
                .context("missing generated snapshot")?;
            ensure!(
                repo.refname_to_id(&generated.reference)?.to_string() == generated.base,
                "generated snapshot ref changed while preparing the commit"
            );
        }
        Ok(())
    }
}

/// Complete metadata publication only when the journaled source commit is authoritative.
pub(crate) fn recover(entries: &[DesiredEntry]) -> Result<()> {
    use crate::materialize::virtual_checkout::generated;
    for entry in entries {
        let Some(mut record) = state::record_for_entry(entry)? else {
            continue;
        };
        let Some(generated) = record.generated.as_mut() else {
            continue;
        };
        let Some(pending) = generated.reverse_pending.clone() else {
            continue;
        };
        state::verify_live_target(entry)?;
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            continue;
        };
        let (repo, _) = git::discover_source(source)?;
        let head = repo.head()?.peel_to_commit()?.id().to_string();
        ensure!(
            head == pending.parent || head == pending.source_commit,
            "source HEAD changed during interrupted reverse commit; inspect HEAD before retrying"
        );
        let reference = repo.refname_to_id(&generated.reference)?.to_string();
        ensure!(
            reference == generated.base
                || (head == pending.source_commit && reference == pending.generated_commit),
            "generated ref changed during interrupted reverse commit"
        );
        if head == pending.source_commit {
            if reference != pending.generated_commit {
                repo.reference_matching(
                    &generated.reference,
                    git2::Oid::from_str(&pending.generated_commit)?,
                    true,
                    git2::Oid::from_str(&generated.base)?,
                    "over reverse patch",
                )?;
            }
            generated.base = pending.generated_commit;
            for destination in pending.destinations {
                generated
                    .source_revisions
                    .insert(destination.clone(), head.clone());
                generated.applied.remove(&destination);
                generated.observed.remove(&destination);
                generated.conflicts.remove(&destination);
                generated.reverse_observed.remove(&destination);
            }
            generated.reverse_observed.extend(pending.observed);
            record.base_oid = head;
            record.last_commit_at = Some(crate::materialize::virtual_checkout::now());
        } else {
            // Pre-HEAD crashes are not whole-command rollback. Never silently bless
            // an index/source write left behind by an interrupted transaction.
            ensure!(
                git2::Oid::hash_object(
                    git2::ObjectType::Blob,
                    &std::fs::read(repo.path().join("index"))?
                )?
                .to_string()
                    == pending.index_before,
                "reverse commit interrupted before HEAD publication; inspect the source index before recovery"
            );
            for (path, expected) in &pending.sources_before {
                validate_relative_ancestors(
                    repo.workdir().context("source has no worktree")?,
                    path,
                )?;
                ensure!(
                    std::fs::symlink_metadata(path)?.is_file()
                        && git2::Oid::hash_object(git2::ObjectType::Blob, &std::fs::read(path)?)?
                            .to_string()
                            == *expected,
                    "reverse commit interrupted before HEAD publication; inspect source template '{}' before recovery",
                    path.display()
                );
            }
        }
        generated.reverse_pending = None;
        generated::persist(entry, &record)?;
    }
    Ok(())
}

pub(crate) fn ordinary_conflicts(entry: &DesiredEntry, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut conflicts = crate::status::virtual_checkout::commit_conflicts(entry, paths)?;
    if let Some(record) = state::record_for_entry(entry)?
        && let Some(generated) = record.generated
    {
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            return Ok(conflicts);
        };
        conflicts.retain(|path| {
            !generated
                .sources
                .values()
                .any(|template| template == &source.join(path))
                && !generated.sources.contains_key(path)
        });
    }
    Ok(conflicts)
}

pub(crate) fn prepare(
    entries: &[DesiredEntry],
    opts: &CommitOptions,
    ctx: &Context,
    overlay: &Overlay,
) -> Result<Prepared> {
    let mut prepared = Prepared::default();
    for entry in entries {
        let Some(record) = state::record_for_entry(entry)? else {
            continue;
        };
        let Some(generated) = record.generated.as_ref() else {
            continue;
        };
        state::verify(entry, &record)?;
        let Provenance::Overlay { source, .. } = &entry.provenance else {
            continue;
        };
        let (repo, managed) = git::discover_source(source)?;
        let workdir = repo.workdir().context("source has no worktree")?;
        let snapshot = repo
            .find_commit(git2::Oid::from_str(&generated.base)?)?
            .tree()?;
        for (destination, template_source) in &generated.sources {
            let target = entry.target.join(destination);
            if !opts.paths.is_empty()
                && !opts
                    .paths
                    .iter()
                    .any(|prefix| target.starts_with(prefix) || template_source.starts_with(prefix))
            {
                continue;
            }
            let mut hunks = String::from("file-level conflict");
            let result = (|| -> Result<Option<File>> {
                validate_relative_ancestors(&entry.target, &target)?;
                let local = prepared.capture(&target)?;
                let base_entry = snapshot.get_path(destination)?;
                let base = repo.find_blob(base_entry.id())?;
                let mode = i32::from(git::on_disk_filemode(&target)?);
                ensure!(
                    mode == base_entry.filemode(),
                    "rendered file mode changes are unsupported"
                );
                if base.content() == local {
                    return Ok(None);
                }
                if let Some(observed) = generated.reverse_observed.get(destination) {
                    let tree = repo.find_commit(git2::Oid::from_str(observed)?)?.tree()?;
                    let consumed = tree.get_path(destination)?.id();
                    if git2::Oid::hash_object(git2::ObjectType::Blob, &local)? == consumed {
                        return Ok(None);
                    }
                    ensure!(
                        base_entry.id() == consumed,
                        "rendered file has unapplied independent source changes; sync before reverse-patching further edits"
                    );
                }
                prepared.selected_sources.insert(template_source.clone());
                if let (Ok(before), Ok(after)) = (text(base.content()), text(&local)) {
                    hunks = edits(before, after)
                        .iter()
                        .enumerate()
                        .map(|(number, edit)| {
                            format!(
                                "hunk {} old lines {}..{}",
                                number + 1,
                                edit.range.start + 1,
                                edit.range.end + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                }
                ensure!(
                    generated.pending.is_none(),
                    "generated snapshot publication is pending; sync first"
                );
                ensure!(
                    !generated.conflicts.contains_key(destination),
                    "generated file has an unresolved reconciliation conflict"
                );
                ensure!(
                    repo.refname_to_id(&generated.reference)?.to_string() == generated.base,
                    "generated snapshot ref differs from its association"
                );
                // Keep logical aliases for selection, but index names are repository-relative physical paths.
                let path = managed.join(template_source.strip_prefix(source)?);
                ensure!(
                    !crate::crypt::policy::protected(&repo, &path)?,
                    "reverse patches for encrypted templates are unsupported"
                );
                validate_relative_ancestors(workdir, &workdir.join(&path))?;
                let physical = prepared.capture(template_source)?;
                let revision = generated
                    .source_revisions
                    .get(destination)
                    .unwrap_or(&generated.source_revision);
                let old_tree = repo.find_commit(git2::Oid::from_str(revision)?)?.tree()?;
                let old_entry = old_tree.get_path(&path)?;
                ensure!(
                    matches!(old_entry.filemode(), 0o100644 | 0o100755),
                    "source template is not a regular file"
                );
                // Canonical Git blobs keep clean filters out of textual source coordinates.
                let old = repo.find_blob(old_entry.id())?;
                let index = repo.index()?;
                ensure!(
                    !index
                        .iter()
                        .any(|item| item.path == path.as_os_str().as_encoded_bytes()
                            && item.flags & 0x3000 != 0),
                    "source index has unresolved conflicts"
                );
                let indexed = index
                    .get_path(&path, 0)
                    .context("source template is missing from the index")?;
                ensure!(
                    matches!(indexed.mode, 0o100644 | 0o100755)
                        && indexed.flags_extended & 0x2000 == 0,
                    "unsupported source index entry"
                );
                let indexed_blob = repo.find_blob(indexed.id)?;
                let canonical =
                    crate::materialize::virtual_checkout::snapshot::canonical_source_entry(
                        &repo, &path,
                    )?
                    .context("source template is missing")?;
                // The comparison helper's scratch ODB is ephemeral; read cleaned bytes in our own scratch.
                let scratch = crate::actions::git::diff::Scratch::new(&repo)?;
                let cleaned = scratch.target_blob(&repo, &path, &physical)?;
                ensure!(
                    cleaned == canonical.0,
                    "source clean filter produced inconsistent content"
                );
                let canonical_blob = scratch.repo.find_blob(cleaned)?;
                ensure!(
                    canonical.1 == indexed.mode as i32,
                    "source template mode has unstaged changes"
                );
                ensure!(
                    ![
                        old.content(),
                        base.content(),
                        &local,
                        indexed_blob.content(),
                        canonical_blob.content()
                    ]
                    .iter()
                    .any(|bytes| bytes.contains(&0)),
                    "binary reverse patches are unsupported"
                );
                let relative = template_source.strip_prefix(&overlay.root)?;
                let policy = crate::overlays::rules::resolve_with_origins(overlay, relative, false);
                let (old_destination, old_render) = crate::desired::content::transform(
                    ctx,
                    overlay,
                    relative,
                    old.content(),
                    old_entry.filemode(),
                )?;
                ensure!(
                    old_destination.strip_prefix(source.strip_prefix(&overlay.root)?)?
                        == destination
                        && old_render.bytes.as_ref() == base.content(),
                    "recorded rendering cannot be reproduced with the current context or rules"
                );
                let original = text(old.content())?;
                // MiniJinja removes one final newline; match literals in that same view,
                // then restore the source-only terminator before computing source edits.
                let literal_source = if policy.template {
                    original
                        .strip_suffix("\r\n")
                        .or_else(|| original.strip_suffix('\n'))
                        .unwrap_or(original)
                } else {
                    original
                };
                let translated = translate(
                    literal_source,
                    text(base.content())?,
                    text(&local)?,
                    policy.template,
                )?;
                let candidate =
                    apply(&lines(literal_source), &translated) + &original[literal_source.len()..];
                let translated = edits(original, &candidate);
                let (_, candidate_render) = crate::desired::content::transform(
                    ctx,
                    overlay,
                    relative,
                    candidate.as_bytes(),
                    old_entry.filemode(),
                )?;
                ensure!(
                    candidate_render.bytes.as_ref() == local,
                    "translated patch does not reproduce the edited rendering"
                );
                let head = repo.head()?.peel_to_tree()?.get_path(&path)?.id();
                // Check each independently selected layer, rather than accidentally staging worktree edits.
                let head_blob = repo.find_blob(head)?;
                merge(
                    text(old.content())?,
                    text(head_blob.content())?,
                    &translated,
                )?;
                let indexed_text = merge(
                    text(old.content())?,
                    text(indexed_blob.content())?,
                    &translated,
                )?;
                let worktree_text = merge(
                    text(old.content())?,
                    text(canonical_blob.content())?,
                    &translated,
                )?;
                let (_, rendered) = crate::desired::content::transform(
                    ctx,
                    overlay,
                    relative,
                    indexed_text.as_bytes(),
                    indexed.mode as i32,
                )?;
                let worktree_id = repo.blob(worktree_text.as_bytes())?;
                let worktree = git::source_worktree_blob(&repo, &path, worktree_id)?;
                // Capture raw bytes too: equivalent canonical content must not hide a prompt-time edit.
                ensure!(
                    std::fs::read(template_source)? == physical,
                    "source changed during reverse-patch preparation"
                );
                Ok(Some(File {
                    target: entry.target.clone(),
                    destination: destination.clone(),
                    source: template_source.clone(),
                    path,
                    mode: indexed.mode,
                    indexed: indexed_text.into_bytes(),
                    worktree,
                    rendered: rendered.bytes.to_vec(),
                    rendered_mode: rendered.mode,
                    expected: canonical,
                    local,
                }))
            })();
            match result {
                Ok(Some(file)) => prepared.files.push(file),
                Ok(None) => {}
                Err(error) => {
                    prepared.selected_sources.insert(template_source.clone());
                    prepared.conflicts.push(format!(
                        "'{}' (source template '{}'): {error:#}; {hunks}; rendered file preserved",
                        target.display(),
                        template_source.display()
                    ));
                }
            }
        }
        prepared.records.push((entry.clone(), record));
    }
    // A template shared by destinations must never receive two independently prepared writes.
    let duplicates: std::collections::BTreeSet<_> = prepared
        .files
        .iter()
        .filter(|file| {
            prepared
                .files
                .iter()
                .filter(|other| other.source == file.source)
                .count()
                > 1
        })
        .map(|file| file.source.clone())
        .collect();
    prepared.files.retain(|file| {
        if duplicates.contains(&file.source) {
            prepared.conflicts.push(format!(
                "source template '{}': multiple destinations propose changes",
                file.source.display()
            ));
            false
        } else {
            true
        }
    });
    prepared.verify()?;
    Ok(prepared)
}

fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).context("binary reverse patches are unsupported")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_edits_leave_expressions_intact() {
        let source = "host={{ host }}\nvalue=old\n";
        let changes = translate(
            source,
            "host=server\nvalue=old\n",
            "host=server\nvalue=new\n",
            true,
        )
        .unwrap();
        assert_eq!(
            apply(&lines(source), &changes),
            "host={{ host }}\nvalue=new\n"
        );
    }

    #[test]
    fn expression_values_and_repeated_literals_are_not_guessed() {
        assert!(translate("host={{ host }}\n", "host=server\n", "host=new\n", true).is_err());
        assert!(translate("x\nx\n", "x\nx\n", "y\nx\n", true).is_err());
        assert!(translate("{{ value }}\nx\n", "x\nx\n", "y\nx\n", true).is_err());
    }

    #[test]
    fn disjoint_source_edits_survive_but_overlapping_edits_conflict() {
        let changes = edits("a\nb\nc\n", "A\nb\nc\n");
        assert_eq!(
            merge("a\nb\nc\n", "a\nb\nC\n", &changes).unwrap(),
            "A\nb\nC\n"
        );
        assert!(merge("a\nb\nc\n", "other\nb\nc\n", &changes).is_err());
    }

    #[test]
    fn literal_insertions_deletions_and_line_endings_are_preserved() {
        for (before, after) in [
            ("a\nb\n", "a\nx\nb\n"),
            ("a\nb\n", "b\n"),
            ("a\r\nb", "A\r\nb"),
        ] {
            let translated = translate(before, before, after, true).unwrap();
            assert_eq!(apply(&lines(before), &translated), after);
        }
    }
}
