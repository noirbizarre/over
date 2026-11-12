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
        if source == ":index" {
            command.arg("--cached");
        } else {
            command.arg(format!("--source={source}"));
        }
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
    #[serde(default)]
    links: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    expected_links: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    modes: BTreeMap<PathBuf, u32>,
    #[serde(default)]
    expected_modes: BTreeMap<PathBuf, u32>,
    #[serde(default)]
    parents: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    origin: Option<PathBuf>,
    #[serde(default)]
    heads: BTreeMap<PathBuf, Option<String>>,
    #[serde(default)]
    identities: BTreeMap<PathBuf, String>,
    #[serde(default)]
    created_directories: Vec<PathBuf>,
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
    begin_commit_with_links(repo, key, files, committed, expected, BTreeMap::new())
}

pub(crate) fn begin_commit_with_links(
    repo: &git2::Repository,
    key: &format::RepositoryKey,
    files: &[PathBuf],
    committed: Option<String>,
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
    expected_links: BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    begin_states(
        repo,
        key,
        files,
        committed,
        expected,
        expected_links,
        BTreeMap::new(),
    )
}

pub(crate) fn begin_commit_with_modes(
    repo: &git2::Repository,
    key: &format::RepositoryKey,
    files: &[PathBuf],
    committed: Option<String>,
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
    modes: BTreeMap<PathBuf, u32>,
) -> Result<()> {
    begin_states(
        repo,
        key,
        files,
        committed,
        expected,
        BTreeMap::new(),
        modes,
    )
}

#[allow(clippy::too_many_arguments)]
fn begin_states(
    repo: &git2::Repository,
    key: &format::RepositoryKey,
    files: &[PathBuf],
    committed: Option<String>,
    expected: BTreeMap<PathBuf, Option<Vec<u8>>>,
    expected_links: BTreeMap<PathBuf, PathBuf>,
    expected_modes: BTreeMap<PathBuf, u32>,
) -> Result<()> {
    // Own all sensitive buffers before the first fallible read, including partial preparation.
    let mut snapshot = Snapshot {
        version: 1,
        administrative: PathBuf::new(),
        committed,
        base_head: None,
        files: BTreeMap::new(),
        expected,
        links: BTreeMap::new(),
        expected_links,
        modes: BTreeMap::new(),
        expected_modes,
        parents: BTreeMap::new(),
        origin: None,
        heads: BTreeMap::new(),
        identities: BTreeMap::new(),
        created_directories: Vec::new(),
    };
    snapshot.administrative = fs::canonicalize(repo.commondir())?;
    snapshot.origin = Some(fs::canonicalize(repo.path())?);
    snapshot.base_head = head_value(repo);
    for worktree in access::worktrees(repo)? {
        snapshot
            .heads
            .insert(fs::canonicalize(worktree.path())?, head_value(&worktree));
    }
    for path in files {
        let mut parent = path.parent().context("snapshot path has no parent")?;
        while !parent.try_exists()? {
            if !snapshot.created_directories.contains(&parent.to_path_buf()) {
                snapshot.created_directories.push(parent.to_path_buf());
            }
            parent = parent.parent().context("snapshot parent has no ancestor")?;
        }
        snapshot.parents.insert(
            path.clone(),
            crate::overlays::mapping::location(
                path.parent().context("snapshot path has no parent")?,
            )?,
        );
        let bytes = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() => {
                snapshot.links.insert(path.clone(), fs::read_link(path)?);
                None
            }
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "transaction destination must be a regular file"
                );
                snapshot
                    .identities
                    .insert(path.clone(), super::transaction::file_identity(path)?);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    snapshot
                        .modes
                        .insert(path.clone(), metadata.permissions().mode() & 0o7777);
                }
                let mut bytes = super::buffer::read_file(path)?;
                Some(std::mem::take(&mut *bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        snapshot.files.insert(path.clone(), bytes);
    }
    for (path, bytes) in &snapshot.expected {
        if bytes.is_some() && !snapshot.modes.contains_key(path) {
            snapshot.expected_modes.entry(path.clone()).or_insert(0o600);
        }
    }
    for (input, source) in &snapshot.expected_links {
        if !snapshot.modes.contains_key(source)
            && let Some(mode) = snapshot.modes.get(input)
        {
            snapshot.expected_modes.insert(source.clone(), *mode);
        }
    }
    let bytes = super::buffer::to_json(&snapshot)?;
    let directory = access::private_dir(repo);
    let mut prepared = tempfile::NamedTempFile::new_in(&directory)?;
    #[cfg(windows)]
    access::windows_acl(prepared.path(), true)?;
    prepared.write_all(&key.encrypt(&bytes, 1)?)?;
    prepared.as_file().sync_all()?;
    // Shared filter guards cannot exclude another prepared policy writer. Never replace its journal.
    prepared
        .persist_noclobber(directory.join("transaction.json"))
        .map_err(|error| error.error)
        .context(
            "another crypt transaction owns the recovery journal; recover it before retrying",
        )?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn head_value(repo: &git2::Repository) -> Option<String> {
    repo.head()
        .ok()
        .and_then(|head| head.target())
        .map(|oid| oid.to_string())
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
    recover_locked(repo, &key)
}

pub(crate) fn recover_locked(repo: &git2::Repository, key: &format::RepositoryKey) -> Result<()> {
    let bytes = key.decrypt(
        &fs::read(access::private_dir(repo).join("transaction.json"))?,
        1,
    )?;
    let mut snapshot: Snapshot = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid crypt recovery journal"))?;
    ensure!(snapshot.version == 1, "unsupported crypt recovery version");
    ensure!(
        snapshot.administrative == fs::canonicalize(repo.commondir())?,
        "recovery journal belongs to another local checkout"
    );
    let origin = snapshot
        .origin
        .as_ref()
        .map(git2::Repository::open)
        .transpose()?;
    let origin = origin.as_ref().unwrap_or(repo);
    let current_head = origin.head().ok().and_then(|head| head.target());
    let committed = snapshot
        .committed
        .as_ref()
        .map(|oid| git2::Oid::from_str(oid))
        .transpose()?;
    let published = if let (Some(expected), Some(current)) = (committed, current_head) {
        expected == current || origin.graph_descendant_of(current, expected)?
    } else {
        false
    };
    for (directory, expected) in &snapshot.heads {
        if Some(directory) == snapshot.origin.as_ref() && published {
            continue;
        }
        ensure!(
            head_value(&git2::Repository::open(directory)?) == *expected,
            "a participating worktree HEAD changed after the interrupted transaction; preserve that commit before recovery"
        );
    }
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
        if let Some(parent) = snapshot.parents.get(path) {
            ensure!(
                crate::overlays::mapping::location(
                    path.parent().context("recovery path has no parent")?
                )? == *parent,
                "transaction path ancestry changed; preserve external data before recovery"
            );
        }
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink()) {
            let link = fs::read_link(path)?;
            ensure!(
                snapshot.links.get(path) == Some(&link)
                    || snapshot.expected_links.get(path) == Some(&link),
                "transaction symlink changed; preserve that edit before recovery"
            );
            continue;
        }
        #[cfg(unix)]
        if let Ok(metadata) = fs::symlink_metadata(path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o7777;
            let known = match (snapshot.modes.get(path), snapshot.expected_modes.get(path)) {
                (None, None) => true,
                (Some(old), expected) => *old == mode || expected == Some(&mode),
                (None, Some(expected)) => *expected == mode,
            };
            ensure!(
                known,
                "transaction permissions changed; preserve that edit before recovery"
            );
        }
        let current = super::buffer::read_optional(path)?;
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
    // A successful forward rename preserves the input inode. Reverse it before
    // generic byte restoration so ACLs, ownership, xattrs and links survive rollback.
    for (input, source) in &snapshot.expected_links {
        if snapshot.files.get(source) != Some(&None)
            || snapshot
                .files
                .get(input)
                .is_none_or(|bytes| bytes.is_none())
        {
            continue;
        }
        let source_exists = fs::symlink_metadata(source).is_ok();
        let input_link = fs::symlink_metadata(input).is_ok_and(|metadata| metadata.is_symlink());
        let input_exists = fs::symlink_metadata(input).is_ok();
        if !source_exists {
            ensure!(
                !input_link,
                "ingested source disappeared independently; preserve remaining data before recovery"
            );
            continue;
        }
        ensure!(
            !input_exists || input_link,
            "an independent source appeared during ingestion; preserve it before recovery"
        );
        let identity = snapshot.identities.get(input).context(
            "journal lacks ingestion inode identity; inspect its saved data before recovery",
        )?;
        ensure!(
            super::transaction::file_identity(source)? == *identity,
            "ingested source inode changed independently; preserve it before recovery"
        );
    }
    for (input, source) in &snapshot.expected_links {
        if snapshot.files.get(source) != Some(&None)
            || snapshot
                .files
                .get(input)
                .is_none_or(|bytes| bytes.is_none())
        {
            continue;
        }
        if !fs::symlink_metadata(source).is_ok() {
            continue;
        }
        if fs::symlink_metadata(input).is_ok() {
            fs::remove_file(input)?;
        }
        fs::rename(source, input).context(
            "could not move the original ingestion inode back; recovery journal retained",
        )?;
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
        let current_link = fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink());
        if let Some(link) = snapshot.links.get(path) {
            if current_link && fs::read_link(path)? == *link {
                continue;
            }
            if fs::symlink_metadata(path).is_ok() {
                fs::remove_file(path)?;
            }
            symlink::symlink_file(link, path)?;
        } else if let Some(bytes) = bytes {
            if let Some(mut lock) = index_locks.remove(path) {
                lock.publish(path, bytes)?;
            } else {
                if current_link {
                    fs::remove_file(path)?;
                }
                // Untouched read-only originals need no replacement during rollback.
                let unchanged = !current_link
                    && fs::read(path)
                        .map(zeroize::Zeroizing::new)
                        .is_ok_and(|current| current.as_slice() == bytes.as_slice());
                if !unchanged {
                    super::transaction::worktree_write(path, bytes, None)?;
                }
                #[cfg(unix)]
                if let Some(mode) = snapshot.modes.get(path) {
                    use std::os::unix::fs::PermissionsExt;
                    if fs::metadata(path)?.permissions().mode() & 0o7777 != *mode {
                        fs::set_permissions(path, fs::Permissions::from_mode(*mode))?;
                    }
                }
            }
        } else if fs::symlink_metadata(path).is_ok() {
            fs::remove_file(path)?;
        }
    }
    snapshot
        .created_directories
        .sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in &snapshot.created_directories {
        match fs::remove_dir(path) {
            Ok(()) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
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
    // Validate clean output in a private ODB before canonical object publication.
    let objects = tempfile::tempdir()?;
    let private = git2::Repository::init_bare(objects.path())?;
    command.env("GIT_OBJECT_DIRECTORY", private.path().join("objects"));
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
    let blob = private.find_blob(oid)?;
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

pub(crate) fn update_inputs_with_ingestion(
    repo: &git2::Repository,
    patterns: &[String],
    inputs: &BTreeMap<PathBuf, PathBuf>,
    ingestion: &mut super::ingest::Ingestion,
) -> Result<()> {
    update_with_ingestion(repo, patterns, true, inputs, &[], Some(ingestion))
}

fn update_with_removals(
    repo: &git2::Repository,
    patterns: &[String],
    add: bool,
    inputs: &BTreeMap<PathBuf, PathBuf>,
    removals: &[PathBuf],
) -> Result<()> {
    update_with_ingestion(repo, patterns, add, inputs, removals, None)
}

fn update_with_ingestion(
    repo: &git2::Repository,
    patterns: &[String],
    add: bool,
    inputs: &BTreeMap<PathBuf, PathBuf>,
    removals: &[PathBuf],
    mut ingestion: Option<&mut super::ingest::Ingestion>,
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
    for path in [
        ".gitattributes",
        ".over/crypt/access.json",
        ".over/.gitattributes",
    ] {
        if let Some(entry) = index.get_path(Path::new(path), 0) {
            ensure!(
                repo.find_blob(entry.id)?.content() == fs::read(root.join(path))?,
                "resolve staged/worktree divergence in {path} before changing policy"
            );
        }
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
    let mut outputs = BTreeMap::new();
    let mut executable_modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
    for path in &paths {
        let physical = inputs.get(path).cloned().unwrap_or_else(|| root.join(path));
        crate::materialize::disposal::validate_relative_ancestors(root, &root.join(path))?;
        let metadata = fs::symlink_metadata(&physical)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "crypt requires a regular working-tree file"
        );
        if crate::materialize::virtual_checkout::snapshot::honors_filemode(repo)? {
            executable_modes.insert(
                path.clone(),
                i32::from(crate::materialize::virtual_checkout::git::on_disk_filemode(
                    &physical,
                )?) as u32,
            );
        }
        let input = super::buffer::read_file(&physical)?;
        working_observations.insert(physical.clone(), input.clone());
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
        outputs.insert(path.clone(), output);
    }
    // A later attribute override must fail before any earlier removal publishes plaintext.
    for (path, output) in &outputs {
        stage_blob(&mut index, repo, path, output)?;
        if let Some(mode) = executable_modes.get(path) {
            let mut entry = index
                .get_path(path, 0)
                .context("prepared protected entry disappeared")?;
            entry.mode = *mode;
            index.add(&entry)?;
        }
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
            super::buffer::read_file(path)? == *before,
            "{} changed during crypt preparation; retry without discarding edits",
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
    let expected_links = ingestion
        .as_ref()
        .map(|ingestion| ingestion.journal(&mut snapshots, &mut expected))
        .unwrap_or_default();
    begin_commit_with_links(repo, &key, &snapshots, None, expected, expected_links)?;
    let result = (|| -> Result<()> {
        super::transaction::worktree_write(&attributes_path, proposed.as_bytes(), None)?;
        lock.replace_while_locked(&repo.path().join("index"), &index_bytes)?;
        for path in removals {
            fs::remove_file(root.join(path))?;
        }
        for (path, bytes) in working_changes {
            super::transaction::worktree_write(&path, &bytes, None)?;
        }
        if let Some(ingestion) = ingestion.as_mut() {
            ingestion.apply()?;
        }
        finish(repo)
    })();
    // Recovery takes its own guard and index lock; release ours before rolling back.
    drop(lock);
    drop(_guard);
    if let Err(error) = result {
        recover(repo).with_context(|| {
            format!("crypt ingestion failed ({error:#}) and rollback requires recovery")
        })?;
        return Err(error);
    }
    Ok(())
}

struct RefreshFile {
    root: PathBuf,
    path: PathBuf,
    before: zeroize::Zeroizing<Vec<u8>>,
    after: zeroize::Zeroizing<Vec<u8>>,
    permissions: fs::Permissions,
    ancestry: crate::materialize::disposal::PathGuard,
}

struct Refresh {
    files: Vec<RefreshFile>,
    heads: BTreeMap<PathBuf, Option<String>>,
    // Keep Git writers out between validation and representation publication.
    _indices: Vec<crate::actions::git::index::IndexLock>,
}

impl Refresh {
    fn prepare(
        repo: &git2::Repository,
        key: &format::RepositoryKey,
        locking: bool,
    ) -> Result<Self> {
        let already_unlocked = !locking && access::local_key(repo).is_ok();
        let mut result = Self {
            files: Vec::new(),
            heads: BTreeMap::new(),
            _indices: Vec::new(),
        };
        for worktree in access::worktrees(repo)? {
            let repo = &worktree;
            let registry = access::load(repo)?;
            ensure!(
                registry.repository == key.repository && registry.generation == key.generation,
                "linked worktrees have incompatible repository/key generations; resolve them before lock/unlock"
            );
            result
                ._indices
                .push(crate::actions::git::index::IndexLock::acquire(
                    &repo.path().join("index"),
                )?);
            result
                .heads
                .insert(repo.path().to_path_buf(), head_value(repo));
            let root = access::root(repo)?;
            let index = repo.index()?;
            ensure!(
                !index.has_conflicts(),
                "resolve protected index conflicts first"
            );
            let paths = tracked(repo)?;
            let effective = attribute_map(repo, &paths, None)?;
            let staged = attribute_map(repo, &paths, Some(":index"))?;
            for path in &paths {
                let entry = index
                    .get_path(path, 0)
                    .context("missing protected index entry")?;
                if path
                    .file_name()
                    .is_some_and(|name| name == ".gitattributes")
                {
                    ensure!(
                        fs::read(root.join(path))? == repo.find_blob(entry.id)?.content(),
                        "resolve unstaged .gitattributes edits before lock/unlock"
                    );
                }
                let blob = if matches!(entry.mode, 0o100644 | 0o100755) {
                    Some(repo.find_blob(entry.id)?)
                } else {
                    None
                };
                let ciphertext = blob
                    .as_ref()
                    .is_some_and(|blob| format::recognized(blob.content()));
                let actual = effective.get(path).copied().unwrap_or(false);
                let indexed = staged.get(path).copied().unwrap_or(false);
                ensure!(
                    !(ciphertext || indexed) || actual,
                    "effective attributes disable protected path {}; resolve policy overrides before lock/unlock",
                    path.display()
                );
                if !actual {
                    continue;
                }
                ensure!(
                    matches!(entry.mode, 0o100644 | 0o100755),
                    "protected worktree entries must be regular files"
                );
                let blob = blob.context("missing protected blob")?;
                if !format::recognized(blob.content()) && !locking {
                    // Historical plaintext must stay recoverable; do not implicitly restage or replace it.
                    eprintln!(
                        "over: historical protected plaintext at {}; run over doctor before staging",
                        path.display()
                    );
                    continue;
                }
                format::inspect(blob.content())?;
                let domain = format::file_domain(blob.content())?;
                let plaintext = key.decrypt(blob.content(), domain)?;
                let physical = root.join(path);
                crate::materialize::disposal::validate_relative_ancestors(root, &physical)?;
                let metadata = fs::symlink_metadata(&physical)?;
                ensure!(
                    metadata.is_file() && !metadata.is_symlink(),
                    "protected working file must be regular"
                );
                let before = super::buffer::read_file(&physical)?;
                if already_unlocked
                    && !format::recognized(&before)
                    && before.as_slice() != plaintext.as_slice()
                {
                    // Repairing an existing unlock must not discard ordinary uncommitted plaintext edits.
                    continue;
                }
                ensure!(
                    before.as_slice() == blob.content()
                        || before.as_slice() == plaintext.as_slice(),
                    "protected file {} has uncommitted edits; stage or save them before lock/unlock",
                    path.display()
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    ensure!(
                        (metadata.permissions().mode() & 0o111 != 0) == (entry.mode == 0o100755),
                        "protected file {} has an unstaged executable-mode edit",
                        path.display()
                    );
                }
                if domain == 1 {
                    continue;
                }
                let after = if locking {
                    zeroize::Zeroizing::new(blob.content().to_vec())
                } else {
                    plaintext
                };
                if before == after {
                    continue;
                }
                ensure!(
                    !metadata.permissions().readonly(),
                    "protected working file is read-only"
                );
                result.files.push(RefreshFile {
                    root: root.to_path_buf(),
                    ancestry: crate::materialize::disposal::PathGuard::capture(&physical)?,
                    path: physical,
                    before,
                    after,
                    permissions: metadata.permissions(),
                });
            }
            let others = git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
            let others: Vec<PathBuf> = others
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| Ok(PathBuf::from(std::str::from_utf8(path)?)))
                .collect::<Result<_>>()?;
            for (path, selected) in attribute_map(repo, &others, None)? {
                if !selected {
                    continue;
                }
                let physical = root.join(&path);
                crate::materialize::disposal::validate_relative_ancestors(root, &physical)?;
                ensure!(
                    fs::symlink_metadata(&physical)?.is_file()
                        && !fs::symlink_metadata(&physical)?.is_symlink(),
                    "untracked protected paths must be regular ciphertext files"
                );
                let bytes = zeroize::Zeroizing::new(fs::read(physical)?);
                if already_unlocked && !format::recognized(&bytes) {
                    continue;
                }
                ensure!(
                    format::recognized(&bytes),
                    "untracked protected plaintext at {}; stage or save it before lock/unlock",
                    path.display()
                );
                key.decrypt(&bytes, format::file_domain(&bytes)?)?;
            }
        }
        result.verify()?;
        Ok(result)
    }

    fn verify(&self) -> Result<()> {
        for (directory, expected) in &self.heads {
            ensure!(
                head_value(&git2::Repository::open(directory)?) == *expected,
                "worktree HEAD changed during lock/unlock; retry"
            );
        }
        for file in &self.files {
            file.ancestry.verify()?;
            crate::materialize::disposal::validate_relative_ancestors(&file.root, &file.path)?;
            let metadata = fs::symlink_metadata(&file.path)?;
            ensure!(
                metadata.is_file()
                    && !metadata.is_symlink()
                    && metadata.permissions() == file.permissions
                    && super::buffer::read_file(&file.path)? == file.before,
                "protected path changed during lock/unlock; preserve edits and retry"
            );
        }
        Ok(())
    }

    fn publish(&self, repo: &git2::Repository, key: &format::RepositoryKey) -> Result<()> {
        self.verify()?;
        if self.files.is_empty() {
            return Ok(());
        }
        let paths = self
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let expected = self
            .files
            .iter()
            .map(|file| (file.path.clone(), Some(file.after.to_vec())))
            .collect();
        begin(repo, key, &paths, expected)?;
        let result = (|| {
            for file in &self.files {
                file.ancestry.verify()?;
                crate::materialize::disposal::validate_relative_ancestors(&file.root, &file.path)?;
                ensure!(
                    super::buffer::read_file(&file.path)? == file.before,
                    "protected file changed before publication"
                );
                super::transaction::worktree_write(&file.path, &file.after, None)?;
            }
            finish(repo)
        })();
        if let Err(error) = result {
            recover_locked(repo, key).with_context(|| {
                format!("{error:#}; lock/unlock rollback requires over crypt recover")
            })?;
            return Err(error);
        }
        Ok(())
    }
}

/// One guarded credential-to-worktree transition; filters never participate in authentication.
pub fn unlock(
    repo: &git2::Repository,
    identity: Option<&Path>,
    passphrase: Option<age::secrecy::SecretString>,
    user: Option<&str>,
    name: Option<&str>,
) -> Result<()> {
    let _guard = access::guard(repo, true)?;
    let key = access::unwrap_key(repo, identity, passphrase, user, name)?;
    let prepared = Refresh::prepare(repo, &key, false)?;
    access::configure_filters(repo)?;
    let previous = if access::local_key_missing(repo)? {
        None
    } else {
        Some(access::recovery_key(repo)?)
    };
    access::install(repo, &key)?;
    if let Err(error) = prepared.publish(repo, &key) {
        // A remaining journal deliberately retains the recovery key, but denies normal access.
        if !access::private_dir(repo)
            .join("transaction.json")
            .try_exists()?
        {
            if let Some(previous) = previous {
                access::install(repo, &previous)?;
            } else {
                access::remove_key(repo)?;
            }
        }
        return Err(error);
    }
    Ok(())
}

pub fn refresh(repo: &git2::Repository, locking: bool) -> Result<()> {
    let _guard = access::guard(repo, true)?;
    let key = access::local_key(repo)?;
    let prepared = Refresh::prepare(repo, &key, locking)?;
    // A successful lock must leave required fail-closed filters, not merely remove a key.
    access::configure_filters(repo)?;
    prepared.publish(repo, &key)?;
    if locking {
        access::remove_key(repo)?;
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

fn native_setting(repo: &git2::Repository, name: &str, boolean: bool) -> Result<Option<String>> {
    let mut command = git_command(repo)?;
    command.arg("config");
    if boolean {
        command.arg("--type=bool");
    }
    let output = command.args(["--get", name]).output()?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    ensure!(
        output.status.success(),
        "could not inspect effective native Git filter configuration"
    );
    let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    Ok(Some(
        std::str::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("native Git filter configuration is not UTF-8"))?
            .to_owned(),
    ))
}

pub fn inspect(repo: &git2::Repository) -> Result<Vec<Diagnostic>> {
    let mut findings = Vec::new();
    // libgit2 does not load every worktree-config extension; inspect what native Git will execute.
    let mut filter_ok =
        native_setting(repo, "filter.over.required", true)?.as_deref() == Some("true");
    for (name, expected) in access::filter_commands()? {
        filter_ok &= native_setting(repo, name, false)?.as_deref() == Some(expected.as_str());
    }
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

pub fn materialized_blob(
    repo: &git2::Repository,
    path: &Path,
    oid: git2::Oid,
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let blob = repo.find_blob(oid)?;
    if protected(repo, path)? {
        super::filter::transform(repo, false, blob.content())
    } else {
        Ok(zeroize::Zeroizing::new(blob.content().to_vec()))
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
    let mut observations = BTreeMap::new();
    let mut observed_heads = BTreeMap::new();
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
        observed_heads.insert(repo.path().to_path_buf(), head_value(repo));
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
            let selected = protected(repo, &path)?;
            let stored_ciphertext = index
                .get_path(&path, 0)
                .map(|entry| repo.find_blob(entry.id))
                .transpose()?
                .is_some_and(|blob| format::recognized(blob.content()));
            ensure!(
                selected || !stored_ciphertext,
                "effective attributes disable an encrypted index path; resolve policy overrides before rotation"
            );
            if !selected {
                continue;
            }
            let physical = root.join(&path);
            crate::materialize::disposal::validate_relative_ancestors(root, &physical)?;
            let metadata = fs::symlink_metadata(&physical)?;
            ensure!(
                metadata.is_file() && !metadata.is_symlink() && !metadata.permissions().readonly(),
                "rotation requires writable regular protected files"
            );
            let stored = if let Some(entry) = index.get_path(&path, 0) {
                repo.find_blob(entry.id)?.content().to_vec()
            } else {
                fs::read(root.join(&path))?
            };
            let domain = format::file_domain(&stored)?;
            let plaintext = old.decrypt(&stored, domain)?;
            let current = zeroize::Zeroizing::new(fs::read(&physical)?);
            ensure!(
                current.as_slice() == stored || current.as_slice() == plaintext.as_slice(),
                "stage protected edits before rotation"
            );
            let ciphertext = new.encrypt(&plaintext, domain)?;
            stage_blob(&mut index, repo, &path, &ciphertext)?;
            if domain == 1 || current.as_slice() == stored {
                observations.insert(physical.clone(), current);
                updates.push((physical, ciphertext));
            }
        }
        let metadata = serde_json::to_vec_pretty(&registry)?;
        stage_blob(
            &mut index,
            repo,
            Path::new(".over/crypt/access.json"),
            &metadata,
        )?;
        let path = access::metadata_path(repo)?;
        ensure!(
            !fs::metadata(&path)?.permissions().readonly(),
            "rotation access metadata is read-only"
        );
        observations.insert(path.clone(), zeroize::Zeroizing::new(fs::read(&path)?));
        updates.push((path, metadata));
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
    for (path, bytes) in &observations {
        ensure!(
            zeroize::Zeroizing::new(fs::read(path)?) == *bytes,
            "protected file changed during rotation preparation"
        );
    }
    for (directory, expected) in &observed_heads {
        ensure!(
            head_value(&git2::Repository::open(directory)?) == *expected,
            "worktree HEAD changed during rotation preparation"
        );
    }
    begin(repo, &old, &snapshots, expected)?;
    for (path, bytes) in updates {
        ensure!(
            observations.get(&path).is_some_and(|old| fs::read(&path)
                .map(zeroize::Zeroizing::new)
                .is_ok_and(|current| current == *old)),
            "protected file changed before rotation publication"
        );
        super::transaction::worktree_write(&path, &bytes, None)?;
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

    fn protected_repository() -> (tempfile::TempDir, git2::Repository, format::RepositoryKey) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let recipient = age::x25519::Identity::generate().to_public().to_string();
        access::add_key(&repo, "owner", "software", Some(&recipient), None, true).unwrap();
        let key = access::local_key(&repo).unwrap();
        fs::write(
            directory.path().join(".gitattributes"),
            "protected filter=over -text\n",
        )
        .unwrap();
        fs::write(directory.path().join("protected"), b"old plaintext").unwrap();
        let mut index = repo.index().unwrap();
        stage_blob(
            &mut index,
            &repo,
            Path::new("protected"),
            &key.encrypt(b"old plaintext", 0).unwrap(),
        )
        .unwrap();
        for path in [
            ".gitattributes",
            ".over/.gitattributes",
            ".over/crypt/access.json",
        ] {
            stage_blob(
                &mut index,
                &repo,
                Path::new(path),
                &fs::read(directory.path().join(path)).unwrap(),
            )
            .unwrap();
        }
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Journal tests", "journal@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        (directory, repo, key)
    }

    fn sibling(repo: &git2::Repository, root: &Path) -> git2::Repository {
        // Test checkout transports ciphertext; the test executable is not an Over CLI filter.
        let mut config = repo.config().unwrap();
        config.remove("filter.over.process").unwrap();
        config.set_str("filter.over.clean", "cat").unwrap();
        config.set_str("filter.over.smudge", "cat").unwrap();
        git(
            repo,
            &["worktree", "add", "-qb", "sibling", root.to_str().unwrap()],
        )
        .unwrap();
        git2::Repository::open(root).unwrap()
    }

    fn advance_head(repo: &git2::Repository, publish: bool) -> git2::Oid {
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let tree = parent.tree().unwrap();
        let signature = git2::Signature::now("Journal tests", "journal@example.invalid").unwrap();
        repo.commit(
            if publish { Some("HEAD") } else { None },
            &signature,
            &signature,
            "concurrent commit",
            &tree,
            &[&parent],
        )
        .unwrap()
    }

    #[test]
    fn a_second_prepared_writer_cannot_replace_or_remove_an_owned_journal() {
        let (directory, repo, key) = protected_repository();
        let path = directory.path().join("transaction-file");
        fs::write(&path, b"old").unwrap();
        let first = access::guard(&repo, false).unwrap();
        let second = access::guard(&repo, false).unwrap();
        begin(
            &repo,
            &key,
            std::slice::from_ref(&path),
            BTreeMap::from([(path.clone(), Some(b"first".to_vec()))]),
        )
        .unwrap();
        let journal = fs::read(access::private_dir(&repo).join("transaction.json")).unwrap();
        assert!(
            begin(
                &repo,
                &key,
                std::slice::from_ref(&path),
                BTreeMap::from([(path.clone(), Some(b"second".to_vec()))])
            )
            .is_err()
        );
        assert_eq!(
            fs::read(access::private_dir(&repo).join("transaction.json")).unwrap(),
            journal
        );
        drop(first);
        drop(second);
        recover(&repo).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"old");
    }

    #[test]
    fn refresh_refuses_an_edit_made_after_its_initial_cleanliness_check() {
        let (directory, repo, key) = protected_repository();
        let prepared = Refresh::prepare(&repo, &key, true).unwrap();
        fs::write(directory.path().join("protected"), b"intervening plaintext").unwrap();
        assert!(prepared.publish(&repo, &key).is_err());
        assert_eq!(
            fs::read(directory.path().join("protected")).unwrap(),
            b"intervening plaintext"
        );
        assert!(access::local_key(&repo).is_ok());
        assert!(!access::private_dir(&repo).join("transaction.json").exists());
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn recovery_is_bound_to_participating_heads_not_the_invoking_worktree(
        #[case] change_after: bool,
    ) {
        let (directory, repo, key) = protected_repository();
        let other_directory = tempfile::tempdir().unwrap();
        let other = sibling(&repo, &other_directory.path().join("worktree"));
        advance_head(&other, true);
        let first = directory.path().join("transaction-file");
        let second = access::root(&other).unwrap().join("transaction-file");
        fs::write(&first, b"old main").unwrap();
        fs::write(&second, b"old sibling").unwrap();
        begin(
            &repo,
            &key,
            &[first.clone(), second.clone()],
            BTreeMap::from([
                (first.clone(), Some(b"prepared".to_vec())),
                (second.clone(), Some(b"prepared".to_vec())),
            ]),
        )
        .unwrap();
        fs::write(&first, b"prepared").unwrap();
        fs::write(&second, b"prepared").unwrap();
        if change_after {
            advance_head(&other, true);
        }
        let result = recover(if change_after { &repo } else { &other });
        if change_after {
            assert!(result.is_err());
            assert_eq!(fs::read(first).unwrap(), b"prepared");
            assert_eq!(fs::read(second).unwrap(), b"prepared");
        } else {
            result.unwrap();
            assert_eq!(fs::read(first).unwrap(), b"old main");
            assert_eq!(fs::read(second).unwrap(), b"old sibling");
        }
    }

    #[test]
    fn recovery_preserves_an_already_published_commit_and_its_source_data() {
        let (directory, repo, key) = protected_repository();
        let path = directory.path().join("transaction-file");
        fs::write(&path, b"old").unwrap();
        let published = advance_head(&repo, false);
        begin_commit(
            &repo,
            &key,
            std::slice::from_ref(&path),
            Some(published.to_string()),
            BTreeMap::from([(path.clone(), Some(b"published data".to_vec()))]),
        )
        .unwrap();
        fs::write(&path, b"published data").unwrap();
        repo.head()
            .unwrap()
            .set_target(published, "publish prepared commit")
            .unwrap();
        advance_head(&repo, true);
        recover(&repo).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"published data");
        assert!(!access::private_dir(&repo).join("transaction.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_intervening_permission_edits_and_restores_original_modes() {
        use std::os::unix::fs::PermissionsExt;
        let (directory, repo, key) = protected_repository();
        let path = directory.path().join("transaction-file");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        begin(
            &repo,
            &key,
            std::slice::from_ref(&path),
            BTreeMap::from([(path.clone(), Some(b"prepared".to_vec()))]),
        )
        .unwrap();
        fs::write(&path, b"prepared").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(recover(&repo).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        recover(&repo).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

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
