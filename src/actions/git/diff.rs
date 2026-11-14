//! Delegate presentation to Git while keeping generated objects and indexes private.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, ensure};
use git2::{Oid, Repository};
use tempfile::TempDir;
use tokio::process::Command;

/// Git's two ordinary diff views. Virtual checkouts supply their own baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum View {
    Unstaged,
    Staged,
}

pub(crate) struct Comparison {
    pub repository: PathBuf,
    pub scope: Vec<PathBuf>,
    pub view: View,
    pub trees: Option<(Oid, Oid)>,
    pub baseline: Option<String>,
    pub scratch: Scratch,
}

/// Owning this value keeps object inputs alive for external and GUI tools.
pub(crate) struct Scratch {
    _directory: TempDir,
    pub repo: Repository,
    index: PathBuf,
    source_objects: PathBuf,
}

impl Scratch {
    pub fn new(source: &Repository) -> Result<Self> {
        let directory = tempfile::tempdir().context("failed to create diff scratch directory")?;
        let repo = Repository::init_bare(directory.path().join("git"))?;
        let index = directory.path().join("index");
        if let Some(path) = source.index()?.path() {
            match std::fs::copy(path, &index) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("failed to snapshot source index"),
            }
        }
        // commondir locates the shared ODB of a linked worktree, not its private gitdir.
        let source_objects = source.commondir().join("objects");
        // hash-object can reuse a blob already present in the source alternate;
        // libgit2 must see the same read-only objects when assembling its trees.
        repo.odb()?.add_disk_alternate(
            source_objects
                .to_str()
                .context("source object directory is not UTF-8")?,
        )?;
        Ok(Self {
            _directory: directory,
            repo,
            index,
            source_objects,
        })
    }

    pub fn target_blob(&self, source: &Repository, path: &Path, content: &[u8]) -> Result<Oid> {
        // Clean filters inherit stderr and can acquire credentials interactively.
        let _pause = crate::ui::progress::pause()?;
        let mut command = std::process::Command::new("git");
        command.current_dir(source.workdir().context("source has no worktree")?);
        for (name, value) in self.environment() {
            command.env(name, value);
        }
        for name in ROUTING_VARIABLES {
            if !matches!(
                name,
                "GIT_INDEX_FILE" | "GIT_OBJECT_DIRECTORY" | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            ) {
                command.env_remove(name);
            }
        }
        // --path selects source attributes/clean filters for this logical path,
        // rather than treating target bytes as an unrelated no-index file.
        let mut child = command
            .args(["hash-object", "-w", "--stdin", "--path"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A verbose clean filter can fill stderr before reading its input;
            // inheriting it avoids deadlocking the synchronous stdin writer.
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to run Git clean filters")?;
        let write_result = child
            .stdin
            .take()
            .context("missing Git input pipe")?
            .write_all(content);
        let output = child.wait_with_output()?;
        ensure!(
            output.status.success(),
            "Git clean filters failed for '{}' ({})",
            path.display(),
            output.status
        );
        write_result?;
        Oid::from_str(std::str::from_utf8(&output.stdout)?.trim()).map_err(Into::into)
    }

    pub(crate) fn environment(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        // A single alternate avoids platform-specific path-list escaping; Git
        // follows the source ODB's own alternates recursively.
        vec![
            ("GIT_OPTIONAL_LOCKS", "0".into()),
            ("GIT_INDEX_FILE", self.index.clone().into_os_string()),
            (
                "GIT_OBJECT_DIRECTORY",
                self.repo.path().join("objects").into_os_string(),
            ),
            (
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                quoted_alternate(&self.source_objects),
            ),
        ]
    }
}

const ROUTING_VARIABLES: [&str; 7] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

fn quoted_alternate(path: &Path) -> std::ffi::OsString {
    // Git accepts C-quoted entries, unlike the shell's platform path-list syntax.
    // Quote backslashes too so a Windows drive path is not parsed as an escape.
    let text = path.as_os_str().to_string_lossy();
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
    .into()
}

impl Comparison {
    pub(crate) fn command(&self, difftool: bool) -> Result<Command> {
        let mut command = Command::new("git");
        command.current_dir(&self.repository);
        // A parent Git command may have exported a different repository/index.
        for variable in ROUTING_VARIABLES {
            command.env_remove(variable);
        }
        // Textconv output is still used, but Git must not persist its cache in
        // source refs/notes while presenting an otherwise read-only comparison.
        let source = Repository::open(&self.repository)?;
        let config = source.config()?;
        let mut caches = config.entries(Some(r"^diff\..*\.cachetextconv$"))?;
        while let Some(entry) = caches.next() {
            command.arg("-c").arg(format!("{}=false", entry?.name()?));
        }
        for (name, value) in self.scratch.environment() {
            command.env(name, value);
        }
        command
            .arg("--literal-pathspecs")
            .arg(if difftool { "difftool" } else { "diff" });
        if !difftool {
            // Porcelain diff normally enables external drivers; be explicit for tree inputs too.
            command.arg("--ext-diff");
        }
        if let Some((left, right)) = self.trees {
            command.arg(left.to_string()).arg(right.to_string());
        } else if self.view == View::Staged {
            command.arg("--cached");
        }
        command.arg("--");
        if self.trees.is_none() {
            command.args(&self.scope);
        }
        Ok(command)
    }

    pub async fn present(&self, difftool: bool) -> Result<()> {
        // No pathspec means the entire repository to Git, never an empty selection.
        if self.scope.is_empty() {
            return Ok(());
        }
        let status = self
            .command(difftool)?
            .status()
            .await
            .context("failed to run Git diff presentation")?;
        // Trusted external diff drivers may return 1 to mean differences,
        // whereas a nonzero difftool status denotes a failed tool invocation.
        ensure!(
            status.success() || (!difftool && status.code() == Some(1)),
            "Git diff presentation failed ({status})"
        );
        Ok(())
    }
}
