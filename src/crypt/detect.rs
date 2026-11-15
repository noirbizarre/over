//! Read-only inspection of committed encryption bootstrap state, before checkout.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, ensure};
use git2::{ErrorCode, Oid, Repository, Tree};

use super::{access, format};

const METADATA_PATH: &str = ".over/crypt/access.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metadata {
    Absent,
    Supported,
    Malformed,
    Unsupported(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiagnosticCode {
    InvalidMetadata,
    UnsupportedMetadata,
    MissingMetadata,
    NoAccessMethod,
    InvalidAccessMethod,
    InvalidCiphertext,
    GenerationMismatch,
    MissingPolicy,
    ProtectedPlaintext,
    ProtectedNonregularPath,
    ProtectedBootstrap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    /// Git path or public user/key label, never content or envelope bytes.
    pub location: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// None denotes an unborn repository, not a failed object lookup.
    pub tree: Option<Oid>,
    pub metadata: Metadata,
    pub protected_paths: Vec<PathBuf>,
    pub ciphertext_paths: Vec<PathBuf>,
    pub usable_access_methods: usize,
    pub diagnostics: Vec<Diagnostic>,
}

impl Detection {
    pub fn is_crypt(&self) -> bool {
        self.metadata != Metadata::Absent
            || !self.protected_paths.is_empty()
            || !self.ciphertext_paths.is_empty()
    }

    /// Structural eligibility only; credentials and authentication are checked at unlock.
    pub fn can_offer_unlock(&self) -> bool {
        self.metadata == Metadata::Supported && self.usable_access_methods > 0
    }

    /// Reject incomplete bootstrap state before a caller materializes any files.
    pub fn validate_checkout(&self) -> Result<()> {
        let blocking: Vec<_> = self
            .diagnostics
            .iter()
            // Independent recovery methods must survive a damaged alternative envelope.
            .filter(|diagnostic| diagnostic.code != DiagnosticCode::InvalidAccessMethod)
            .map(|diagnostic| {
                format!(
                    "{:?} at {}: {}",
                    diagnostic.code,
                    diagnostic.location.escape_default(),
                    diagnostic.message
                )
            })
            .collect();
        ensure!(
            blocking.is_empty(),
            "invalid committed Over crypt state: {}",
            blocking.join("; ")
        );
        Ok(())
    }

    fn diagnostic(
        &mut self,
        code: DiagnosticCode,
        location: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(Diagnostic {
            code,
            location: location.into(),
            message: message.into(),
        });
    }
}

/// Inspect HEAD without consulting a working tree, index, local key, or filter configuration.
pub fn inspect(repo: &Repository) -> Result<Detection> {
    match repo.head() {
        Ok(head) => inspect_tree(repo, &head.peel_to_tree()?),
        Err(error) if error.code() == ErrorCode::UnbornBranch => Ok(empty_detection(None)),
        Err(error) => Err(error).context("could not resolve HEAD for crypt detection"),
    }
}

fn empty_detection(tree: Option<Oid>) -> Detection {
    Detection {
        tree,
        metadata: Metadata::Absent,
        protected_paths: Vec::new(),
        ciphertext_paths: Vec::new(),
        usable_access_methods: 0,
        diagnostics: Vec::new(),
    }
}

/// Inspect exactly this committed tree. Scratch Git state is isolated from the source.
pub fn inspect_tree(repo: &Repository, tree: &Tree<'_>) -> Result<Detection> {
    let mut result = empty_detection(Some(tree.id()));
    let mut entries = BTreeMap::new();
    collect_entries(repo, tree, Path::new(""), &mut entries)?;
    let registry = if let Some((oid, mode)) = entries.get(Path::new(METADATA_PATH)) {
        let parsed = (|| -> Result<access::Registry> {
            ensure!(regular(*mode), "access metadata must be a regular Git blob");
            let (size, _) = repo.odb()?.read_header(*oid)?;
            ensure!(
                size <= access::METADATA_LIMIT,
                "repository access metadata exceeds the supported size limit"
            );
            access::parse_registry(repo.find_blob(*oid)?.content())
        })();
        match parsed {
            Ok(registry) => {
                result.metadata = Metadata::Supported;
                for (user, keys) in &registry.users {
                    for (name, key) in keys {
                        if usable(key) {
                            result.usable_access_methods += 1;
                        } else {
                            result.diagnostic(
                                DiagnosticCode::InvalidAccessMethod,
                                format!("{user}/{name}"),
                                "invalid public recipient or Age envelope",
                            );
                        }
                    }
                }
                if result.usable_access_methods == 0 {
                    result.diagnostic(DiagnosticCode::NoAccessMethod, METADATA_PATH, "no structurally usable repository key envelope; repair committed access metadata");
                }
                Some(registry)
            }
            Err(error) => {
                let code = if let Some(version) =
                    error.downcast_ref::<access::UnsupportedRegistryVersion>()
                {
                    result.metadata = Metadata::Unsupported(version.0);
                    DiagnosticCode::UnsupportedMetadata
                } else {
                    result.metadata = Metadata::Malformed;
                    DiagnosticCode::InvalidMetadata
                };
                result.diagnostic(code, METADATA_PATH, error.to_string());
                None
            }
        }
    } else {
        None
    };
    // Include directories in the inventory so a tree at the registry path is not mistaken for absence.
    let paths: Vec<_> = entries
        .iter()
        .filter(|(_, (_, mode))| *mode != 0o040000)
        .map(|(path, _)| path.clone())
        .collect();
    let source = ObjectSource::new(repo)?;
    let attributes = committed_attributes(&source, tree.id(), &paths)?;
    let blobs: BTreeSet<_> = entries
        .iter()
        .filter(|(path, (_, mode))| regular(*mode) && *path != Path::new(METADATA_PATH))
        .map(|(_, (oid, _))| *oid)
        .collect();
    let headers = source.blob_headers(&blobs)?;
    for (path, (oid, mode)) in &entries {
        if *mode == 0o040000 {
            continue;
        }
        let protected = attributes.get(path).copied().unwrap_or(false);
        let location = path.to_string_lossy().into_owned();
        if protected {
            result.protected_paths.push(path.clone());
            if !regular(*mode) {
                result.diagnostic(
                    DiagnosticCode::ProtectedNonregularPath,
                    &location,
                    "protected paths must be regular Git blobs",
                );
            }
            if path
                .file_name()
                .is_some_and(|name| name == ".gitattributes")
                || path.starts_with(".over/crypt")
            {
                result.diagnostic(
                    DiagnosticCode::ProtectedBootstrap,
                    &location,
                    "bootstrap metadata and policy must remain unencrypted",
                );
            }
        }
        // Registry validation above already rejects encrypted or oversized metadata;
        // loading it again here would bypass the pre-allocation size check.
        if !regular(*mode) || path == Path::new(METADATA_PATH) {
            continue;
        }
        let (bytes, length) = &headers[oid];
        // A lone NUL or other short prefix is common binary content, not crypt identification.
        let ciphertext = bytes.starts_with(format::MAGIC);
        if ciphertext {
            result.ciphertext_paths.push(path.clone());
            if let Err(error) = format::inspect_header(bytes, *length) {
                result.diagnostic(
                    DiagnosticCode::InvalidCiphertext,
                    &location,
                    error.to_string(),
                );
            } else if registry
                .as_ref()
                .is_some_and(|registry| bytes[10..format::HEADER_LEN] != registry.generation)
            {
                result.diagnostic(
                    DiagnosticCode::GenerationMismatch,
                    &location,
                    "ciphertext uses a different repository key generation",
                );
            }
            if !protected {
                result.diagnostic(
                    DiagnosticCode::MissingPolicy,
                    &location,
                    "ciphertext has no committed filter=over policy",
                );
            }
        } else if protected {
            result.diagnostic(
                DiagnosticCode::ProtectedPlaintext,
                &location,
                "protected Git blob is not Over ciphertext",
            );
        }
    }
    if result.is_crypt() && result.metadata == Metadata::Absent {
        result.diagnostic(
            DiagnosticCode::MissingMetadata,
            METADATA_PATH,
            "protected paths or ciphertext exist without committed access metadata",
        );
    }
    result
        .diagnostics
        .sort_by(|left, right| (&left.location, left.code).cmp(&(&right.location, right.code)));
    Ok(result)
}

fn regular(mode: i32) -> bool {
    matches!(mode, 0o100644 | 0o100755)
}

fn collect_entries(
    repo: &Repository,
    tree: &Tree<'_>,
    parent: &Path,
    entries: &mut BTreeMap<PathBuf, (Oid, i32)>,
) -> Result<()> {
    for entry in tree {
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(entry.name_bytes())
        };
        #[cfg(not(unix))]
        let name = std::ffi::OsStr::new(entry.name().context("invalid Git path encoding")?);
        let path = parent.join(name);
        entries.insert(path.clone(), (entry.id(), entry.filemode()));
        if entry.kind() == Some(git2::ObjectType::Tree) {
            collect_entries(repo, &repo.find_tree(entry.id())?, &path, entries)?;
        }
    }
    Ok(())
}

fn usable(key: &access::AccessKey) -> bool {
    let mut payload = key.envelope.as_slice();
    let Ok(envelope) = age::Decryptor::new_buffered(&mut payload) else {
        return false;
    };
    let passphrase = envelope.is_scrypt();
    drop(envelope);
    // A wrapped repository key contains 96 bytes plus the stream authentication tag.
    // Header parsing alone would accept an envelope with its entire payload removed.
    if payload.len() < 96 + 16 {
        return false;
    }
    match key.recipient.as_deref() {
        None => passphrase,
        Some(text) => {
            !passphrase
                && (text.parse::<age::x25519::Recipient>().is_ok()
            || text.parse::<age::tag::Recipient>().is_ok()
            || text.parse::<age::tagpq::Recipient>().is_ok()
            || text.parse::<age::ssh::Recipient>().is_ok()
                    // This syntax-only Bech32 parser also accepts native age1pq1 descriptors;
                    // unlike RecipientPluginV1 construction, it never resolves an executable.
                    || text.parse::<age::plugin::Recipient>().is_ok())
        }
    }
}

fn isolated_git(empty: &Path) -> Command {
    let mut command = Command::new("git");
    // Remove the entire namespace: indexed config injection and future routing variables count too.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .arg("-c")
        .arg(format!("core.attributesFile={}", empty.display()));
    command
}

fn committed_attributes(
    source: &ObjectSource,
    tree: Oid,
    paths: &[PathBuf],
) -> Result<BTreeMap<PathBuf, bool>> {
    if paths.is_empty() {
        return Ok(BTreeMap::new());
    }
    // --source requires newer Git than Debian's supported devcontainer image provides.
    // This index belongs only to the disposable repository, never to the inspected clone.
    let output = source
        .command()
        .args(["read-tree", &tree.to_string()])
        .output()?;
    ensure!(
        output.status.success(),
        "could not prepare committed attribute inspection"
    );
    let mut child = source
        .command()
        .args(["check-attr", "--cached", "-z", "--stdin", "filter"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .context("missing attribute inspection stdin")?;
    let output = std::thread::scope(|scope| -> Result<_> {
        let writer = scope.spawn(move || -> Result<()> {
            for path in paths {
                input.write_all(path.as_os_str().as_encoded_bytes())?;
                input.write_all(&[0])?;
            }
            Ok(())
        });
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("attribute inspection input failed"))??;
        Ok(output)
    })?;
    ensure!(
        output.status.success(),
        "could not inspect committed encryption attributes"
    );
    let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    ensure!(
        fields.len() == paths.len() * 3 + 1 && fields.last() == Some(&&b""[..]),
        "invalid committed attribute response"
    );
    let mut result = BTreeMap::new();
    for (path, fields) in paths.iter().zip(fields.as_chunks::<3>().0) {
        ensure!(
            fields[0] == path.as_os_str().as_encoded_bytes() && fields[1] == b"filter",
            "unexpected committed attribute response"
        );
        result.insert(path.clone(), fields[2] == b"over");
    }
    Ok(result)
}

struct ObjectSource {
    // Own the scratch directory until all attribute and object readers have exited.
    _scratch: tempfile::TempDir,
    empty: PathBuf,
    git_dir: PathBuf,
    alternate: String,
}

impl ObjectSource {
    fn new(repo: &Repository) -> Result<Self> {
        let scratch = tempfile::tempdir()?;
        let empty = scratch.path().join("empty");
        std::fs::write(&empty, [])?;
        let git_dir = scratch.path().join("repository.git");
        let output = isolated_git(&empty)
            .args(["init", "--bare", "--template="])
            .arg(&git_dir)
            .output()?;
        ensure!(
            output.status.success(),
            "could not initialize isolated attribute inspection"
        );
        let objects = repo.commondir().join("objects").canonicalize()?;
        // Git alternates use C quoting, not shell quoting; quote separators and arbitrary UTF-8 bytes.
        let mut alternate = String::from("\"");
        for &byte in objects.as_os_str().as_encoded_bytes() {
            match byte {
                b'"' | b'\\' => {
                    alternate.push('\\');
                    alternate.push(byte as char);
                }
                0x20..=0x7e => alternate.push(byte as char),
                _ => alternate.push_str(&format!("\\{byte:03o}")),
            }
        }
        alternate.push('"');
        Ok(Self {
            _scratch: scratch,
            empty,
            git_dir,
            alternate,
        })
    }

    fn command(&self) -> Command {
        let mut command = isolated_git(&self.empty);
        command
            .arg("--git-dir")
            .arg(&self.git_dir)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.alternate);
        command
    }

    fn blob_headers(&self, blobs: &BTreeSet<Oid>) -> Result<BTreeMap<Oid, (Vec<u8>, usize)>> {
        if blobs.is_empty() {
            return Ok(BTreeMap::new());
        }
        // Native Git streams loose/packed blobs; libgit2's fallback reader buffers entire objects.
        let mut child = self
            .command()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .context("missing object inspection stdin")?;
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .context("missing object inspection stdout")?,
        );
        std::thread::scope(|scope| -> Result<_> {
            let writer = scope.spawn(move || -> Result<()> {
                for oid in blobs {
                    writeln!(input, "{oid}")?;
                }
                Ok(())
            });
            let result = (|| -> Result<_> {
                let mut headers = BTreeMap::new();
                for oid in blobs {
                    let mut line = String::new();
                    output.by_ref().take(256).read_line(&mut line)?;
                    let fields: Vec<_> = line.split_whitespace().collect();
                    ensure!(
                        fields.len() == 3 && fields[0] == oid.to_string() && fields[1] == "blob",
                        "invalid object inspection response"
                    );
                    let length: usize = fields[2].parse().context("invalid object length")?;
                    let mut prefix = vec![0; length.min(format::HEADER_LEN)];
                    output.read_exact(&mut prefix)?;
                    // Drain each body through a fixed-size buffer so the next batch record remains aligned.
                    let remaining = (length - prefix.len()) as u64;
                    ensure!(
                        std::io::copy(&mut output.by_ref().take(remaining), &mut std::io::sink())?
                            == remaining,
                        "truncated Git blob"
                    );
                    let mut newline = [0];
                    output.read_exact(&mut newline)?;
                    ensure!(newline == *b"\n", "invalid object inspection terminator");
                    headers.insert(*oid, (prefix, length));
                }
                Ok(headers)
            })();
            // Close the read end on errors before joining the writer, avoiding pipe deadlocks.
            drop(output);
            let status = child.wait()?;
            let written = writer
                .join()
                .map_err(|_| anyhow::anyhow!("object inspection input failed"))?;
            let result = result?;
            written?;
            ensure!(status.success(), "could not inspect committed Git blobs");
            Ok(result)
        })
    }
}
