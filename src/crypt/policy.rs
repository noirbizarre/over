//! Canonical Git attributes and storage transitions.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{access, atomic_write, format};

pub fn git(repo: &git2::Repository, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = git_command(repo)?.args(arguments).output()?;
    ensure!(
        output.status.success(),
        "Git encryption operation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn git_command(repo: &git2::Repository) -> Result<Command> {
    let mut command = Command::new("git");
    command.arg("-C").arg(access::root(repo)?);
    // Parent hooks/scratch Git processes must not redirect canonical repository operations.
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_ATTR_SOURCE",
    ] {
        command.env_remove(name);
    }
    Ok(command)
}

pub fn protected(repo: &git2::Repository, path: &Path) -> Result<bool> {
    let path = path.to_str().context("protected path is not UTF-8")?;
    let attributes = git(repo, &["check-attr", "-z", "filter", "--", path])?;
    Ok(attributes.split(|b| *b == 0).nth(2) == Some(b"over"))
}

pub fn is_policy_extension(before: &[u8], after: &[u8]) -> bool {
    let Some(extension) = after.strip_prefix(before) else {
        return false;
    };
    let Ok(extension) = std::str::from_utf8(extension) else {
        return false;
    };
    extension.lines().all(|line| {
        if line.is_empty()
            || matches!(
                line,
                ".gitattributes -filter" | ".over/crypt/** -filter -text"
            )
        {
            return true;
        }
        line.strip_suffix(" filter=over -text")
            .or_else(|| line.strip_suffix(" -filter -text"))
            .is_some_and(|pattern| serde_json::from_str::<String>(pattern).is_ok())
    })
}

fn tracked(repo: &git2::Repository) -> Result<Vec<PathBuf>> {
    git(repo, &["ls-files", "-z"])?
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|bytes| {
            Ok(PathBuf::from(
                std::str::from_utf8(bytes).context("crypt requires UTF-8 Git paths")?,
            ))
        })
        .collect()
}

fn attribute_map(
    repo: &git2::Repository,
    paths: &[PathBuf],
    source: Option<&str>,
) -> Result<BTreeMap<PathBuf, bool>> {
    let mut command = git_command(repo)?;
    command.args(["check-attr", "-z", "--stdin"]);
    if let Some(source) = source {
        command.arg(format!("--source={source}"));
    }
    let mut child = command
        .arg("filter")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().context("missing Git attribute stdin")?;
    let output = std::thread::scope(|scope| -> Result<_> {
        let writer = scope.spawn(move || -> Result<()> {
            for path in paths {
                input.write_all(
                    path.to_str()
                        .context("crypt requires UTF-8 paths")?
                        .as_bytes(),
                )?;
                input.write_all(&[0])?;
            }
            Ok(())
        });
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git attribute input failed"))??;
        Ok(output)
    })?;
    ensure!(
        output.status.success(),
        "could not inspect effective Git encryption attributes"
    );
    let fields: Vec<_> = output.stdout.split(|b| *b == 0).collect();
    let mut result = BTreeMap::new();
    for triple in fields.as_chunks::<3>().0 {
        result.insert(
            PathBuf::from(std::str::from_utf8(triple[0])?),
            triple[2] == b"over",
        );
    }
    Ok(result)
}

/// Recovery snapshots are encrypted, including working files and index metadata.
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    administrative: PathBuf,
    committed: Option<String>,
    base_head: Option<String>,
    files: BTreeMap<PathBuf, Option<Vec<u8>>>,
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        for bytes in self.files.values_mut().flatten() {
            bytes.zeroize();
        }
        for bytes in self.expected.values_mut().flatten() {
            bytes.zeroize();
        }
    }
}

fn begin(
    repo: &git2::Repository,
    key: &format::RepositoryKey,
    files: &[PathBuf],
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Result<()> {
    begin_commit(repo, key, files, None, expected)
}

pub(crate) fn begin_commit(
    repo: &git2::Repository,
    key: &format::RepositoryKey,
    files: &[PathBuf],
    committed: Option<String>,
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> Result<()> {
    let mut saved = BTreeMap::new();
    for path in files {
        let bytes = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        saved.insert(path.clone(), bytes);
    }
    let snapshot = Snapshot {
        version: 1,
        administrative: fs::canonicalize(repo.commondir())?,
        committed,
        base_head: repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| oid.to_string()),
        files: saved,
        expected,
    };
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(&snapshot)?);
    atomic_write(
        &access::private_dir(repo).join("transaction.json"),
        &key.encrypt(&bytes, 1)?,
    )
}

pub(crate) fn finish(repo: &git2::Repository) -> Result<()> {
    fs::remove_file(access::private_dir(repo).join("transaction.json"))?;
    #[cfg(unix)]
    fs::File::open(access::private_dir(repo))?.sync_all()?;
    Ok(())
}

pub fn recover(repo: &git2::Repository) -> Result<()> {
    let _guard = access::raw_guard(repo, true)?;
    let key = access::recovery_key(repo)?;
    let bytes = key.decrypt(
        &fs::read(access::private_dir(repo).join("transaction.json"))?,
        1,
    )?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid crypt recovery journal"))?;
    ensure!(snapshot.version == 1, "unsupported crypt recovery version");
    ensure!(
        snapshot.administrative == fs::canonicalize(repo.commondir())?,
        "recovery journal belongs to another local checkout"
    );
    let current_head = repo.head().ok().and_then(|head| head.target());
    let committed = snapshot
        .committed
        .as_ref()
        .map(|oid| git2::Oid::from_str(oid))
        .transpose()?;
    let published = if let (Some(expected), Some(current)) = (committed, current_head) {
        expected == current || repo.graph_descendant_of(current, expected)?
    } else {
        false
    };
    if published {
        // HEAD publication is authoritative; never undo source changes after a successful commit.
        return finish(repo);
    }
    ensure!(
        current_head.map(|oid| oid.to_string()) == snapshot.base_head,
        "HEAD changed after the interrupted crypt transaction; inspect and preserve that commit/branch before recovery"
    );
    let mut index_locks = BTreeMap::new();
    for path in snapshot.files.keys().filter(|path| {
        path.starts_with(repo.commondir()) && path.file_name().is_some_and(|name| name == "index")
    }) {
        let lock = crate::actions::git::index::IndexLock::acquire(path)
            .context("index is locked; if the original process has exited, inspect and remove its stale index.lock before retrying over crypt recover")?;
        index_locks.insert(path.clone(), lock);
    }
    // Refuse intervening edits before restoring even the first path.
    for (path, old) in &snapshot.files {
        let current = match fs::read(path) {
            Ok(bytes) => Some(zeroize::Zeroizing::new(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        ensure!(
            current.as_ref().map(|bytes| bytes.as_slice()) == old.as_deref()
                || snapshot
                    .expected
                    .get(path)
                    .is_some_and(
                        |new| current.as_ref().map(|bytes| bytes.as_slice()) == new.as_deref()
                    ),
            "{} changed after the interrupted transaction; preserve that edit before recovering",
            path.display()
        );
    }
    let mut restore: Vec<_> = snapshot.files.iter().collect();
    // Restore attributes/files first, publishing the coherent old index last.
    restore.sort_by_key(|(path, _)| index_locks.contains_key(*path));
    for (path, bytes) in restore {
        // Authenticated journals may include explicitly owned virtual destinations outside Git.
        ensure!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir),
            "invalid recovery path"
        );
        if let Some(bytes) = bytes {
            if let Some(mut lock) = index_locks.remove(path) {
                lock.publish(path, bytes)?;
            } else {
                atomic_write(path, bytes)?;
            }
        } else if path.try_exists()? {
            fs::remove_file(path)?;
        }
    }
    finish(repo)
}

fn stage_blob(
    index: &mut git2::Index,
    repo: &git2::Repository,
    path: &Path,
    bytes: &[u8],
) -> Result<()> {
    let mut entry = index.get_path(path, 0).unwrap_or(git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: git2::Oid::ZERO_SHA1,
        flags: 0,
        flags_extended: 0,
        path: path
            .to_str()
            .context("crypt requires UTF-8 paths")?
            .as_bytes()
            .to_vec(),
    });
    ensure!(
        matches!(entry.mode, 0o100644 | 0o100755),
        "crypt only supports regular files"
    );
    entry.id = repo.blob(bytes)?;
    entry.file_size = bytes.len().try_into()?;
    // Converted blobs must invalidate stat data so Git runs clean on the next comparison.
    entry.mtime = git2::IndexTime::new(0, 0);
    index.add(&entry)?;
    Ok(())
}

fn index_bytes(repo: &git2::Repository, index: &git2::Index) -> Result<Vec<u8>> {
    let temporary = tempfile::tempdir_in(repo.path())?;
    let path = temporary.path().join("index");
    let mut prepared = git2::Index::open(&path)?;
    for entry in index.iter() {
        prepared.add(&entry)?;
    }
    prepared.write()?;
    Ok(fs::read(path)?)
}

/// Run the real clean filter with proposed attributes, without publishing plaintext files.
fn clean_with_policy(
    repo: &git2::Repository,
    scratch: &Path,
    path: &Path,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let attributes = git_command(repo)?
        .arg("-C")
        .arg(scratch)
        .args(["check-attr", "-z", "filter", "--"])
        .arg(path)
        .env("GIT_WORK_TREE", scratch)
        .env("GIT_DIR", repo.path())
        .output()?;
    ensure!(
        attributes.status.success() && attributes.stdout.split(|b| *b == 0).nth(2) == Some(b"over"),
        "effective attributes override crypt policy; resolve nested/global/info attributes before staging"
    );
    let mut command = git_command(repo)?;
    command.arg("-C").arg(scratch);
    // Force the known Over implementation before -w can publish any object.
    for (name, value) in access::filter_commands()? {
        command.arg("-c").arg(format!("{name}={value}"));
    }
    let mut child = command
        .args([
            "-c",
            "filter.over.required=true",
            "hash-object",
            "-w",
            "--stdin",
            "--path",
        ])
        .arg(path)
        .env("GIT_WORK_TREE", scratch)
        .env("GIT_DIR", repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("missing Git stdin")?;
    // Pipe writing runs concurrently with output collection to avoid large-input deadlock.
    let output = std::thread::scope(|scope| -> Result<_> {
        let writer = scope.spawn(move || stdin.write_all(plaintext));
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("Git input writer failed"))??;
        Ok(output)
    })?;
    ensure!(
        output.status.success(),
        "Git clean filter failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oid = git2::Oid::from_str(std::str::from_utf8(&output.stdout)?.trim())?;
    let blob = repo.find_blob(oid)?;
    format::inspect(blob.content())?;
    Ok(blob.content().to_vec())
}

pub fn update(repo: &git2::Repository, patterns: &[String], add: bool) -> Result<()> {
    update_inputs(repo, patterns, add, &BTreeMap::new())
}

pub fn update_inputs(
    repo: &git2::Repository,
    patterns: &[String],
    add: bool,
    inputs: &BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    update_with_removals(repo, patterns, add, inputs, &[])
}

fn update_with_removals(
    repo: &git2::Repository,
    patterns: &[String],
    add: bool,
    inputs: &BTreeMap<PathBuf, PathBuf>,
    removals: &[PathBuf],
) -> Result<()> {
    access::configure_filters(repo)?;
    let _guard = access::guard(repo, false)?;
    let key = access::local_key(repo)?;
    let root = access::root(repo)?;
    let original_index = crate::actions::git::index::read_index(&repo.path().join("index"))?;
    let attributes_path = root.join(".gitattributes");
    let attributes = fs::read_to_string(&attributes_path).or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Ok(String::new())
        } else {
            Err(e)
        }
    })?;
    let mut proposed = attributes.clone();
    if !proposed.ends_with('\n') && !proposed.is_empty() {
        proposed.push('\n');
    }
    let mut paths = Vec::new();
    for pattern in patterns {
        ensure!(
            !pattern.starts_with('/')
                && !pattern.split('/').any(|part| part == "..")
                && !pattern.contains(['\n', '\r', '\0']),
            "crypt paths must be repository-relative"
        );
        let literal = inputs.contains_key(Path::new(pattern)) || root.join(pattern).is_file();
        let rule = if literal {
            let escaped: String = pattern
                .chars()
                .flat_map(|c| {
                    if "*?[\\".contains(c) {
                        vec!['\\', c]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            format!("/{escaped}")
        } else if root.join(pattern).is_dir() {
            format!("{}/**", pattern.trim_end_matches('/'))
        } else {
            pattern.clone()
        };
        let quoted = serde_json::to_string(&rule)?;
        proposed.push_str(&format!(
            "{quoted} {} -text\n",
            if add { "filter=over" } else { "-filter" }
        ));
        let pathspec = if literal {
            format!(":(literal){pattern}")
        } else {
            pattern.clone()
        };
        let matches = git(
            repo,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
                "--",
                &pathspec,
            ],
        )?;
        for path in matches.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            paths.push(PathBuf::from(std::str::from_utf8(path)?));
        }
    }
    proposed.push_str(".gitattributes -filter\n.over/crypt/** -filter -text\n");
    paths.sort();
    paths.dedup();
    paths.extend(inputs.keys().cloned());
    paths.sort();
    paths.dedup();
    ensure!(
        !paths
            .iter()
            .any(|p| p == Path::new(".gitattributes") || p.starts_with(".over/crypt")),
        "bootstrap attributes and access envelopes cannot be protected"
    );
    let mut index = repo.index()?;
    ensure!(
        !index.has_conflicts(),
        "resolve Git index conflicts before changing crypt policy"
    );
    if let Some(entry) = index.get_path(Path::new(".gitattributes"), 0) {
        ensure!(
            repo.find_blob(entry.id)?.content() == attributes.as_bytes(),
            "stage or resolve existing .gitattributes edits before changing policy"
        );
    }
    let scratch = tempfile::tempdir()?;
    // Only public configuration is copied; content is sent to Git via stdin.
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".git")
    {
        let entry = entry?;
        if entry.file_name() == ".gitattributes" && entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(root)?;
            atomic_write(&scratch.path().join(relative), &fs::read(entry.path())?)?;
        }
    }
    atomic_write(&scratch.path().join(".gitattributes"), proposed.as_bytes())?;
    atomic_write(
        &scratch.path().join(".over/crypt/access.json"),
        &fs::read(access::metadata_path(repo)?)?,
    )?;
    let mut working_changes = BTreeMap::new();
    let mut working_observations = BTreeMap::new();
    for path in &paths {
        let physical = inputs.get(path).cloned().unwrap_or_else(|| root.join(path));
        crate::materialize::disposal::validate_relative_ancestors(root, &root.join(path))?;
        let metadata = fs::symlink_metadata(&physical)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "crypt requires a regular working-tree file"
        );
        let input = zeroize::Zeroizing::new(fs::read(&physical)?);
        let output = zeroize::Zeroizing::new(if add {
            clean_with_policy(repo, scratch.path(), path, &input)?
        } else {
            let attributes = git_command(repo)?
                .arg("-C")
                .arg(scratch.path())
                .args(["check-attr", "-z", "filter", "--"])
                .arg(path)
                .env("GIT_WORK_TREE", scratch.path())
                .env("GIT_DIR", repo.path())
                .output()?;
            ensure!(
                attributes.status.success()
                    && attributes.stdout.split(|b| *b == 0).nth(2) == Some(b"unset"),
                "effective attributes override policy removal; resolve nested/local rules before staging plaintext"
            );
            if format::recognized(&input) {
                let plaintext = key.decrypt(&input, 0)?;
                working_observations.insert(physical.clone(), input.clone());
                working_changes.insert(physical, plaintext.clone());
                plaintext.to_vec()
            } else {
                input.to_vec()
            }
        });
        if add {
            key.decrypt(&output, format::file_domain(&output)?)?;
        }
        stage_blob(&mut index, repo, path, &output)?;
    }
    stage_blob(
        &mut index,
        repo,
        Path::new(".gitattributes"),
        proposed.as_bytes(),
    )?;
    for path in [".over/crypt/access.json", ".over/.gitattributes"] {
        stage_blob(
            &mut index,
            repo,
            Path::new(path),
            &fs::read(root.join(path))?,
        )?;
    }
    for path in removals {
        index.remove_path(path)?;
    }
    let mut lock = crate::actions::git::index::IndexLock::acquire(&repo.path().join("index"))?;
    ensure!(
        crate::actions::git::index::read_index(&repo.path().join("index"))? == original_index,
        "index changed during crypt preparation; retry"
    );
    ensure!(
        fs::read_to_string(&attributes_path).unwrap_or_default() == attributes,
        "attributes changed during crypt preparation; retry"
    );
    let mut snapshots = vec![attributes_path.clone(), repo.path().join("index")];
    for (path, before) in &working_observations {
        ensure!(
            zeroize::Zeroizing::new(fs::read(path)?) == *before,
            "{} changed during policy removal; retry without discarding edits",
            path.display()
        );
    }
    snapshots.extend(removals.iter().map(|path| root.join(path)));
    snapshots.extend(working_changes.keys().cloned());
    let index_bytes = index_bytes(repo, &index)?;
    let mut expected = BTreeMap::from([
        (attributes_path.clone(), Some(proposed.as_bytes().to_vec())),
        (repo.path().join("index"), Some(index_bytes.clone())),
    ]);
    expected.extend(removals.iter().map(|path| (root.join(path), None)));
    expected.extend(
        working_changes
            .iter()
            .map(|(path, bytes)| (path.clone(), Some(bytes.to_vec()))),
    );
    begin(repo, &key, &snapshots, expected)?;
    atomic_write(&attributes_path, proposed.as_bytes())?;
    lock.publish(&repo.path().join("index"), &index_bytes)?;
    for path in removals {
        fs::remove_file(root.join(path))?;
    }
    for (path, bytes) in working_changes {
        atomic_write(&path, &bytes)?;
    }
    finish(repo)
}

pub fn refresh(repo: &git2::Repository, locking: bool) -> Result<()> {
    let _guard = access::guard(repo, true)?;
    let key = access::local_key(repo)?;
    let mut writes = Vec::new();
    for worktree in access::worktrees(repo)? {
        let repo = &worktree;
        ensure!(
            access::load(repo)?.generation == key.generation,
            "linked worktrees have incompatible encryption generations; resolve them before locking"
        );
        let root = access::root(repo)?;
        let index = repo.index()?;
        ensure!(
            !index.has_conflicts(),
            "resolve protected index conflicts first"
        );
        for path in tracked(repo)? {
            if !protected(repo, &path)? {
                continue;
            }
            let entry = index
                .get_path(&path, 0)
                .context("missing protected index entry")?;
            let blob = repo.find_blob(entry.id)?;
            format::inspect(blob.content())?;
            let domain = format::file_domain(blob.content())?;
            let plaintext = key.decrypt(blob.content(), domain)?;
            let current = zeroize::Zeroizing::new(fs::read(root.join(&path))?);
            ensure!(
                current.as_slice() == blob.content() || current.as_slice() == plaintext.as_slice(),
                "protected file {} has uncommitted edits; stage or save them before lock/unlock",
                path.display()
            );
            if domain == 0 {
                writes.push((
                    root.join(path),
                    if locking {
                        blob.content().to_vec()
                    } else {
                        plaintext.to_vec()
                    },
                ));
            }
        }
    }
    begin(
        repo,
        &key,
        &writes
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>(),
        writes
            .iter()
            .map(|(path, bytes)| (path.clone(), Some(bytes.clone())))
            .collect(),
    )?;
    for (path, bytes) in &writes {
        let bytes = zeroize::Zeroizing::new(bytes.clone());
        atomic_write(path, &bytes)?;
    }
    finish(repo)?;
    if locking {
        fs::remove_file(access::private_dir(repo).join("key"))?;
    }
    Ok(())
}

/// Severity is typed so a filename beginning with "ERROR" cannot change exit status.
#[derive(Debug, Clone)]
pub enum Diagnostic {
    Healthy(String),
    Warning(String),
    Error(String),
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Healthy(message) | Self::Warning(message) | Self::Error(message)) = self;
        formatter.write_str(message)
    }
}

pub fn inspect(repo: &git2::Repository) -> Result<Vec<Diagnostic>> {
    let mut findings = Vec::new();
    let config = repo.config()?;
    let filter_ok = config.get_bool("filter.over.required").unwrap_or(false)
        && access::filter_commands()?.iter().all(|(name, expected)| {
            config
                .get_string(name)
                .is_ok_and(|actual| actual == *expected)
        });
    let index = repo.index()?;
    let head = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let key = match access::local_key(repo) {
        Ok(key) => Some(key),
        Err(error) => {
            if !access::local_key_missing(repo)? {
                findings.push(Diagnostic::Error(format!("ERROR repository key state: {error:#}; inspect private permissions and unlock the matching repository generation")));
            }
            None
        }
    };
    let mut paths: std::collections::BTreeSet<_> = tracked(repo)?.into_iter().collect();
    if let Some(head) = &head {
        head.walk(git2::TreeWalkMode::PreOrder, |prefix, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob) {
                match entry.name() {
                    Ok(name) => {
                        paths.insert(Path::new(prefix).join(name));
                    }
                    Err(_) => return git2::TreeWalkResult::Abort,
                }
            }
            git2::TreeWalkResult::Ok
        })?;
    }
    let paths: Vec<_> = paths.into_iter().collect();
    let effective = attribute_map(repo, &paths, None)?;
    let committed = if head.is_some() {
        attribute_map(repo, &paths, Some("HEAD"))?
    } else {
        BTreeMap::new()
    };
    // Root policy intent is checked in isolation so local/nested overrides cannot hide it.
    let scratch = tempfile::tempdir()?;
    let policy_repo = git2::Repository::init(scratch.path())?;
    let empty_attributes = scratch.path().join("empty-attributes");
    fs::write(&empty_attributes, [])?;
    policy_repo.config()?.set_str(
        "core.attributesFile",
        empty_attributes
            .to_str()
            .context("attribute path is not UTF-8")?,
    )?;
    let attributes = match fs::read(access::root(repo)?.join(".gitattributes")) {
        Ok(attributes) => Some(attributes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("could not inspect canonical encryption policy"),
    };
    if let Some(attributes) = attributes {
        atomic_write(&scratch.path().join(".gitattributes"), &attributes)?;
        for line in String::from_utf8_lossy(&attributes)
            .lines()
            .filter(|line| line.contains("filter=over") && !line.trim_start().starts_with('#'))
        {
            findings.push(Diagnostic::Healthy(format!(
                "policy .gitattributes: {line}"
            )));
        }
    }
    let intended = attribute_map(&policy_repo, &paths, None)?;
    let mut history = repo.revwalk()?;
    if repo.head().is_ok() {
        history.push_head()?;
    }
    let history: Vec<_> = history.take(16).collect::<std::result::Result<_, _>>()?;
    let mut history_budget = 256usize;
    for path in paths {
        let actual = effective.get(&path).copied().unwrap_or(false);
        let declared = intended.get(&path).copied().unwrap_or(false)
            || path == Path::new(".over/secrets.yaml");
        let previously_protected = committed.get(&path).copied().unwrap_or(false);
        if !actual && !declared && !previously_protected {
            continue;
        }
        if declared && !actual {
            findings.push(Diagnostic::Error(format!("ERROR {} policy: effective attributes disable declared protection; resolve nested/global/info attribute overrides before over crypt add", path.display())));
        }
        if actual
            && (path
                .file_name()
                .is_some_and(|name| name == ".gitattributes")
                || path.starts_with(".over/crypt"))
        {
            findings.push(Diagnostic::Error(format!("ERROR {} policy: bootstrap metadata must remain readable without filtering; correct .gitattributes before cloning or locking", path.display())));
        }
        if !filter_ok {
            findings.push(Diagnostic::Error(format!("ERROR {}: required local Over filters missing/incorrect; run over unlock before staging", path.display())));
        }
        for (location, oid) in [
            ("index", index.get_path(&path, 0).map(|e| e.id)),
            (
                "HEAD",
                head.as_ref()
                    .and_then(|tree| tree.get_path(&path).ok())
                    .map(|entry| entry.id()),
            ),
        ] {
            if location == "index" && !actual && !declared {
                findings.push(Diagnostic::Healthy(format!(
                    "{} index: unprotected by current policy; future commits may store plaintext",
                    path.display()
                )));
                continue;
            }
            let Some(oid) = oid else {
                continue;
            };
            let blob = repo.find_blob(oid)?;
            if location == "HEAD"
                && format::inspect(blob.content()).is_ok()
                && key
                    .as_ref()
                    .is_some_and(|key| blob.content()[10..format::HEADER_LEN] != key.generation)
            {
                findings.push(Diagnostic::Warning(format!("WARNING {} HEAD: supported ciphertext from an older generation; authentication needs that historical key. Commit the staged rotation to advance current HEAD.", path.display())));
                continue;
            }
            let result = format::inspect(blob.content()).and_then(|()| {
                if let Some(key) = &key {
                    key.decrypt(blob.content(), format::file_domain(blob.content())?)
                        .map(|_| ())
                } else {
                    Ok(())
                }
            });
            match result {
                Ok(()) => findings.push(Diagnostic::Healthy(format!("{} {location}: ciphertext ({})", path.display(), if key.is_some() { "authenticated" } else { "authentication unverified while locked" }))),
                Err(error) => findings.push(Diagnostic::Error(format!("ERROR {} {location}: {error}; plaintext exposure or corrupt protection; restore from a trusted copy, then over crypt add. Current encryption does not erase historical exposure.", path.display()))),
            }
        }
        findings.push(Diagnostic::Healthy(format!(
            "{} working-tree: {}",
            path.display(),
            if key.is_some() {
                "plaintext is expected while unlocked"
            } else {
                "locked; inspect retained ciphertext"
            }
        )));
        for oid in &history {
            if history_budget == 0 {
                break;
            }
            history_budget -= 1;
            let tree = repo.find_commit(*oid)?.tree()?;
            if let Ok(entry) = tree.get_path(&path)
                && entry.kind() == Some(git2::ObjectType::Blob)
                && !repo
                    .find_blob(entry.id())?
                    .content()
                    .starts_with(format::MAGIC)
            {
                findings.push(Diagnostic::Warning(format!("WARNING {} history: known plaintext at {oid}; encryption/rotation does not erase old commits, clones, backups or remotes. Review exposure and rotate the underlying secret; history cleanup is separate.", path.display())));
                break;
            }
        }
    }
    if !findings.is_empty() {
        findings.push(Diagnostic::Healthy("History coverage: at most 16 commits and 256 path/blob checks; a complete reachable-history audit is a separate future capability.".to_owned()));
    }
    Ok(findings)
}

/// Evaluate attributes from the actual candidate tree, not unstaged worktree rules.
pub fn validate_tree(repo: &git2::Repository, tree: &git2::Tree<'_>) -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let index_path = temporary.path().join("index");
    let mut index = git2::Index::open(&index_path)?;
    index.read_tree(tree)?;
    index.write()?;
    for entry in index.iter() {
        let path = std::str::from_utf8(&entry.path).context("crypt requires UTF-8 paths")?;
        let attributes = git_command(repo)?
            .args(["check-attr", "--cached", "-z", "filter", "--", path])
            .env("GIT_INDEX_FILE", &index_path)
            .output()?;
        ensure!(
            attributes.status.success(),
            "could not validate candidate encryption attributes"
        );
        if attributes.stdout.split(|b| *b == 0).nth(2) == Some(b"over") {
            let blob = repo.find_blob(entry.id)?;
            format::inspect(blob.content()).with_context(|| {
                format!(
                    "candidate commit exposes protected plaintext at {path}; run over crypt add"
                )
            })?;
        }
    }
    Ok(())
}

pub fn materialized_blob(repo: &git2::Repository, path: &Path, oid: git2::Oid) -> Result<Vec<u8>> {
    let blob = repo.find_blob(oid)?;
    if protected(repo, path)? {
        super::filter::transform(repo, false, blob.content())
    } else {
        Ok(blob.content().to_vec())
    }
}

pub fn rotate(
    repo: &git2::Repository,
    revoke: Option<(&str, Option<&str>)>,
    mut password: impl FnMut(&str) -> Result<age::secrecy::SecretString>,
) -> Result<()> {
    let _guard = access::guard(repo, true)?;
    let old = access::local_key(repo)?;
    let mut new = format::RepositoryKey::generate()?;
    new.repository = old.repository;
    let mut registry = access::load(repo)?;
    let original_registry = serde_json::to_vec(&registry)?;
    let revoked_passwords: Vec<_> = if let Some((user, name)) = revoke {
        registry
            .users
            .get(user)
            .into_iter()
            .flat_map(|keys| keys.iter())
            .filter(|(key, method)| {
                name.is_none_or(|selected| selected == key.as_str()) && method.recipient.is_none()
            })
            .map(|(_, method)| method.clone())
            .collect()
    } else {
        Vec::new()
    };
    if let Some((user, name)) = revoke {
        let keys = registry.users.get_mut(user).context("unknown user")?;
        if let Some(name) = name {
            ensure!(keys.remove(name).is_some(), "unknown key");
        } else {
            keys.clear();
        }
    }
    ensure!(
        registry.users.values().any(|keys| !keys.is_empty()),
        "cannot revoke the only recovery key; add a trusted recovery key first"
    );
    for (user, keys) in &mut registry.users {
        for (name, key) in keys {
            let passphrase = if key.recipient.is_none() {
                Some(password(&format!(
                    "Passphrase for retained key {user}/{name}"
                ))?)
            } else {
                None
            };
            if let Some(passphrase) = &passphrase {
                access::verify_passphrase(key, passphrase.clone(), &old)?;
                ensure!(
                    !revoked_passwords
                        .iter()
                        .any(|revoked| access::verify_passphrase(
                            revoked,
                            passphrase.clone(),
                            &old
                        )
                        .is_ok()),
                    "a retained passphrase also unlocks a revoked key; replace that reused credential before revocation"
                );
            }
            *key = access::envelope(&new, key.recipient.as_deref(), passphrase)?;
        }
    }
    registry.generation = new.generation;
    let mut updates = Vec::new();
    let mut indices = Vec::new();
    let worktrees = access::worktrees(repo)?;
    for worktree in &worktrees {
        let repo = worktree;
        ensure!(
            serde_json::to_vec(&access::load(repo)?)? == original_registry,
            "linked worktrees have divergent access metadata; reconcile it before rotation"
        );
        let lock = crate::actions::git::index::IndexLock::acquire(&repo.path().join("index"))?;
        let root = access::root(repo)?;
        let mut index = repo.index()?;
        ensure!(
            !index.has_conflicts(),
            "resolve index conflicts before rotation"
        );
        let mut protected_paths = tracked(repo)?;
        if root.join(".over/secrets.yaml").try_exists()?
            && !protected_paths.contains(&PathBuf::from(".over/secrets.yaml"))
        {
            protected_paths.push(PathBuf::from(".over/secrets.yaml"));
        }
        for path in protected_paths {
            if !protected(repo, &path)? {
                continue;
            }
            let stored = if let Some(entry) = index.get_path(&path, 0) {
                repo.find_blob(entry.id)?.content().to_vec()
            } else {
                fs::read(root.join(&path))?
            };
            let domain = format::file_domain(&stored)?;
            let plaintext = old.decrypt(&stored, domain)?;
            let current = zeroize::Zeroizing::new(fs::read(root.join(&path))?);
            ensure!(
                current.as_slice() == stored || current.as_slice() == plaintext.as_slice(),
                "stage protected edits before rotation"
            );
            let ciphertext = new.encrypt(&plaintext, domain)?;
            stage_blob(&mut index, repo, &path, &ciphertext)?;
            if domain == 1 || current.as_slice() == stored {
                updates.push((root.join(path), ciphertext));
            }
        }
        let metadata = serde_json::to_vec_pretty(&registry)?;
        stage_blob(
            &mut index,
            repo,
            Path::new(".over/crypt/access.json"),
            &metadata,
        )?;
        updates.push((access::metadata_path(repo)?, metadata));
        indices.push((repo, index_bytes(repo, &index)?, lock));
    }
    let mut snapshots: Vec<_> = updates.iter().map(|(path, _)| path.clone()).collect();
    snapshots.extend(indices.iter().map(|(repo, _, _)| repo.path().join("index")));
    let mut expected: BTreeMap<_, _> = updates
        .iter()
        .map(|(path, bytes)| (path.clone(), Some(bytes.clone())))
        .collect();
    expected.extend(
        indices
            .iter()
            .map(|(repo, bytes, _)| (repo.path().join("index"), Some(bytes.clone()))),
    );
    begin(repo, &old, &snapshots, expected)?;
    for (path, bytes) in updates {
        atomic_write(&path, &bytes)?;
    }
    for (repo, bytes, mut lock) in indices {
        lock.publish(&repo.path().join("index"), &bytes)?;
    }
    finish(repo)?;
    // A crash here leaves a stale local key, recoverable through the new Age envelopes.
    access::install(repo, &new)?;
    eprintln!(
        "Rotation protects new state only. Old commits, clones, backups and copied plaintext remain accessible with old keys."
    );
    Ok(())
}

pub fn migrate(repo: &git2::Repository) -> Result<()> {
    let root = access::root(repo)?;
    let key = access::local_key(repo)?;
    let legacy = crate::secrets::CryptoService::default();
    let mut prepared = BTreeMap::new();
    let mut originals = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    let index = repo.index()?;
    let mut candidates = tracked(repo)?;
    let legacy_store = root.join(".over/secrets");
    if legacy_store.try_exists()? {
        ensure!(
            !fs::symlink_metadata(&legacy_store)?
                .file_type()
                .is_symlink(),
            "legacy secret store must not be a symlink"
        );
        for entry in fs::read_dir(&legacy_store)? {
            let entry = entry?;
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "age")
            {
                candidates.push(entry.path().strip_prefix(root)?.to_path_buf());
            } else {
                anyhow::bail!(
                    "legacy secret store contains unrecognized files; preserve and resolve them before migration"
                );
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    for path in candidates {
        if path.extension().is_none_or(|extension| extension != "age") {
            continue;
        }
        let physical = root.join(&path);
        crate::materialize::disposal::validate_relative_ancestors(root, &physical)?;
        ensure!(
            fs::symlink_metadata(&physical)?.is_file()
                && !fs::symlink_metadata(&physical)?.file_type().is_symlink(),
            "legacy inputs must be real regular files"
        );
        let ciphertext = fs::read(&physical)?;
        if let Some(entry) = index.get_path(&path, 0) {
            ensure!(
                repo.find_blob(entry.id)?.content() == ciphertext,
                "legacy file {} differs from its index snapshot; preserve/stage those edits before migration",
                path.display()
            );
        }
        let plaintext = legacy.decrypt(&ciphertext, true)?;
        if path.starts_with(".over/secrets") {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("invalid legacy secret name")?;
            crate::secrets::SecretName::parse(name)?;
            secrets.insert(name.to_owned(), plaintext);
        } else {
            let destination = path.with_extension("");
            let encrypted = key.encrypt(plaintext.expose(), 0)?;
            if root.join(&destination).try_exists()? {
                let current = zeroize::Zeroizing::new(fs::read(root.join(&destination))?);
                ensure!(
                    current.as_slice() == encrypted || current.as_slice() == plaintext.expose(),
                    "migration destination {} has different content; preserve edits and resolve the collision first",
                    destination.display()
                );
            }
            prepared.insert(destination, encrypted);
        }
        originals.insert(path, ciphertext);
    }
    if !secrets.is_empty() {
        let values: BTreeMap<_, _> = secrets
            .iter()
            .map(|(name, value)| (name, value.expose()))
            .collect();
        let plaintext = zeroize::Zeroizing::new(serde_json::to_vec(&(values, 1u32))?);
        let ciphertext = key.encrypt(&plaintext, 1)?;
        if root.join(".over/secrets.yaml").try_exists()? {
            ensure!(
                fs::read(root.join(".over/secrets.yaml"))? == ciphertext,
                "legacy/new secret stores differ; preserve and merge their contents explicitly before migration"
            );
        }
        prepared.insert(PathBuf::from(".over/secrets.yaml"), ciphertext);
    }
    if originals.is_empty() && access::private_dir(repo).join("legacy-backup").is_dir() {
        if legacy_store.try_exists()? {
            fs::remove_dir(&legacy_store)
                .context("inspect remaining legacy files before completing migration")?;
        }
        return refresh(repo, false);
    }
    ensure!(!originals.is_empty(), "no legacy Age data found");
    // Authenticate every input before publishing anything. Originals remain as ciphertext backups.
    for (path, bytes) in &originals {
        let backup = access::private_dir(repo).join("legacy-backup").join(path);
        crate::materialize::disposal::validate_relative_ancestors(
            &access::private_dir(repo),
            &backup,
        )?;
        if backup.try_exists()? {
            ensure!(
                fs::read(&backup)? == *bytes,
                "legacy backup differs; preserve both versions before retrying migration"
            );
        } else {
            atomic_write(&backup, bytes)?;
        }
    }
    for (path, bytes) in &prepared {
        atomic_write(&root.join(path), bytes)?;
    }
    let patterns: Vec<_> = prepared
        .keys()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    let tracked: std::collections::BTreeSet<_> = tracked(repo)?.into_iter().collect();
    let removals: Vec<_> = originals
        .keys()
        .filter(|path| tracked.contains(*path))
        .cloned()
        .collect();
    update_with_removals(repo, &patterns, true, &BTreeMap::new(), &removals)?;
    for path in originals.keys().filter(|path| !tracked.contains(*path)) {
        fs::remove_file(root.join(path))?;
    }
    if legacy_store.try_exists()? {
        fs::remove_dir(&legacy_store)
            .context("legacy store contains unrecognized files; inspect before cleanup")?;
    }
    refresh(repo, false)?;
    eprintln!(
        "Migration staged ciphertext and policy. Original Age ciphertext is preserved under Git administrative over-crypt/legacy-backup. Review and commit explicitly; old history is unchanged."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_recovery_restores_the_old_state_but_refuses_later_edits() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temporary.path()).unwrap();
        let identity = age::x25519::Identity::generate();
        access::add_key(
            &repo,
            "owner",
            "software",
            Some(&identity.to_public().to_string()),
            None,
            true,
        )
        .unwrap();
        let key = access::local_key(&repo).unwrap();
        let path = temporary.path().join("file");
        fs::write(&path, b"old sentinel").unwrap();
        begin(
            &repo,
            &key,
            std::slice::from_ref(&path),
            BTreeMap::from([(path.clone(), Some(b"new sentinel".to_vec()))]),
        )
        .unwrap();
        let journal = fs::read(access::private_dir(&repo).join("transaction.json")).unwrap();
        assert!(!journal.windows(8).any(|bytes| bytes == b"sentinel"));
        fs::write(&path, b"intervening edit").unwrap();
        assert!(recover(&repo).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"intervening edit");
        fs::write(&path, b"new sentinel").unwrap();
        recover(&repo).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old sentinel");
        assert!(!access::private_dir(&repo).join("transaction.json").exists());
        assert!(access::local_key(&repo).is_ok());
    }

    #[test]
    fn unavailable_retained_passphrases_abort_rotation_without_changing_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temporary.path()).unwrap();
        access::add_key(
            &repo,
            "owner",
            "password",
            None,
            Some("right credential".into()),
            true,
        )
        .unwrap();
        let before = fs::read(access::metadata_path(&repo).unwrap()).unwrap();
        assert!(rotate(&repo, None, |_| Ok("wrong credential".into())).is_err());
        assert_eq!(
            fs::read(access::metadata_path(&repo).unwrap()).unwrap(),
            before
        );
        assert!(!access::private_dir(&repo).join("transaction.json").exists());
    }

    #[test]
    fn a_reused_passphrase_cannot_preserve_access_through_a_revoked_keys_alias() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temporary.path()).unwrap();
        access::add_key(
            &repo,
            "owner",
            "first",
            None,
            Some("shared credential".into()),
            true,
        )
        .unwrap();
        access::add_key(
            &repo,
            "owner",
            "second",
            None,
            Some("shared credential".into()),
            false,
        )
        .unwrap();
        let before = fs::read(access::metadata_path(&repo).unwrap()).unwrap();
        assert!(
            rotate(&repo, Some(("owner", Some("first"))), |_| Ok(
                "shared credential".into()
            ))
            .is_err()
        );
        assert_eq!(
            fs::read(access::metadata_path(&repo).unwrap()).unwrap(),
            before
        );
    }

    #[test]
    fn duplicate_public_recipients_cannot_create_an_empty_user_or_revocation_alias() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(temporary.path()).unwrap();
        let recipient = age::x25519::Identity::generate().to_public().to_string();
        access::add_key(&repo, "owner", "software", Some(&recipient), None, true).unwrap();
        let before = fs::read(access::metadata_path(&repo).unwrap()).unwrap();
        assert!(access::add_key(&repo, "alias", "software", Some(&recipient), None, true).is_err());
        assert_eq!(
            fs::read(access::metadata_path(&repo).unwrap()).unwrap(),
            before
        );
    }
}
