//! Standard Age inputs and isolated cryptographic attempts, independent of repository policy.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use age::Callbacks;
use anyhow::{Context as _, Result, ensure};
use zeroize::Zeroizing;

const INPUT_LIMIT: u64 = 16 * 1024 * 1024;
pub(crate) const ENVELOPE_LIMIT: usize = 16 * 1024;

fn bounded_read(reader: impl Read, limit: u64) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "Age input exceeds size limit");
    Ok(bytes)
}

pub fn recipient<C: Callbacks>(
    text: &str,
    callbacks: C,
) -> Result<(String, Box<dyn age::Recipient>)> {
    if let Ok(key) = text.parse::<age::x25519::Recipient>() {
        return Ok((key.to_string(), Box::new(key)));
    }
    if let Ok(key) = text.parse::<age::tag::Recipient>() {
        return Ok((key.to_string(), Box::new(key)));
    }
    if let Ok(key) = text.parse::<age::tagpq::Recipient>() {
        return Ok((key.to_string(), Box::new(key)));
    }
    if let Ok(key) = text.parse::<age::ssh::Recipient>() {
        return Ok((key.to_string(), Box::new(key)));
    }
    let key = text
        .parse::<age::plugin::Recipient>()
        .map_err(|_| anyhow::anyhow!("invalid or unsupported Age recipient"))?;
    let plugin = age::plugin::RecipientPluginV1::new(
        key.plugin(),
        std::slice::from_ref(&key),
        &[],
        callbacks,
    )
    .with_context(|| format!("install age-plugin-{} and put it on PATH", key.plugin()))?;
    Ok((key.to_string(), Box::new(plugin)))
}

pub fn recipients_file<C: Callbacks>(path: &Path, callbacks: C) -> Result<Vec<String>> {
    ensure!(
        path != Path::new("-"),
        "recipient inputs require a file path"
    );
    let bytes = bounded_read(
        File::open(path).context("could not open Age recipient file")?,
        INPUT_LIMIT,
    )?;
    let text = std::str::from_utf8(&bytes).context("Age recipient file is not UTF-8")?;
    let mut recipients = Vec::new();
    for (line, text) in text.lines().enumerate() {
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        // Age permits authorized_keys lists containing unrelated SSH key types.
        if matches!(
            text.parse::<age::ssh::Recipient>(),
            Err(age::ssh::ParseRecipientKeyError::Ignore
                | age::ssh::ParseRecipientKeyError::Unsupported(_))
        ) {
            callbacks.display_message("Ignoring an unsupported SSH key type in the recipient file");
            continue;
        }
        ensure!(
            is_native_pq(text) || recipient(text, age::NoCallbacks).is_ok(),
            "unavailable or invalid recipient in {} on line {} (check plugin installation)",
            path.display(),
            line + 1
        );
        recipients.push(text.to_owned());
    }
    ensure!(
        !recipients.is_empty(),
        "Age recipient file contains no supported recipients"
    );
    Ok(recipients)
}

fn is_native_pq(text: &str) -> bool {
    text.starts_with("age1pq1")
}

pub(crate) fn wrap<C: Callbacks>(
    bytes: &[u8],
    text: &str,
    callbacks: C,
) -> Result<(String, Vec<u8>)> {
    if is_native_pq(text) {
        let output = cli(&["--encrypt", "--recipient", text], bytes, ENVELOPE_LIMIT)?;
        ensure!(!output.is_empty(), "Age returned an empty envelope");
        age::Decryptor::new(output.as_slice()).context("Age returned an invalid envelope")?;
        return Ok((text.to_owned(), output.to_vec()));
    }
    let (canonical, recipient) = recipient(text, callbacks)?;
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(recipient.as_ref())).map_err(|_| {
            anyhow::anyhow!("Age recipient wrapping failed; check plugin, device and confirmation")
        })?;
    let mut output = Vec::new();
    let mut writer = encryptor.wrap_output(&mut output).map_err(|_| {
        anyhow::anyhow!("Age recipient wrapping failed; check plugin, device and confirmation")
    })?;
    writer.write_all(bytes)?;
    writer
        .finish()
        .map_err(|_| anyhow::anyhow!("Age recipient wrapping failed"))?;
    ensure!(
        output.len() <= ENVELOPE_LIMIT,
        "Age envelope exceeds size limit"
    );
    Ok((canonical, output))
}

pub(crate) enum Identity {
    Library(Box<dyn age::Identity>),
    NativePq(Zeroizing<String>),
    Deferred {
        initialize: Option<Box<dyn FnOnce() -> Result<Identities>>>,
        loaded: Option<Identities>,
    },
}

impl Identity {
    pub(crate) fn accepts(&self, recipient: &str) -> bool {
        // Native PQ entries need no subprocess for envelopes addressed to other key types.
        !matches!(self, Self::NativePq(_)) || is_native_pq(recipient)
    }

    pub(crate) fn decrypt(&mut self, envelope: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match self {
            Self::Library(identity) => {
                let decryptor = age::Decryptor::new(envelope)?;
                let reader = match decryptor.decrypt(std::iter::once(identity.as_ref())) {
                    Ok(reader) => reader,
                    Err(age::DecryptError::NoMatchingKeys) => return Ok(None),
                    Err(error) => {
                        // A plugin stub can locate multiple keys; one envelope's error must
                        // not disable the same stub for another independently configured key.
                        return Err(anyhow::anyhow!(decrypt_error(&error)));
                    }
                };
                // Read through authenticated EOF; a filled key-sized buffer alone is insufficient.
                bounded_read(reader, 96).map(Some)
            }
            Self::NativePq(identity) => {
                // Only public ciphertext touches disk; private identity material travels on stdin.
                let mut ciphertext = tempfile::NamedTempFile::new()?;
                ciphertext.write_all(envelope)?;
                let path = ciphertext
                    .path()
                    .to_str()
                    .context("non-UTF-8 temporary path")?;
                cli(
                    &["--decrypt", "--identity", "-", path],
                    identity.as_bytes(),
                    96,
                )
                .map(Some)
            }
            Self::Deferred { initialize, loaded } => {
                if let Some(initialize) = initialize.take() {
                    // Cache successful file decryption and failed/cancelled input for this invocation.
                    *loaded = Some(initialize()?);
                }
                let Some(loaded) = loaded else {
                    return Ok(None);
                };
                let mut failure = None;
                for identity in &mut loaded.entries {
                    match identity.decrypt(envelope) {
                        Ok(Some(plaintext)) => return Ok(Some(plaintext)),
                        Ok(None) => (),
                        Err(error) => failure = Some(error),
                    }
                }
                match failure {
                    Some(error) => Err(error),
                    None => Ok(None),
                }
            }
        }
    }
}

fn decrypt_error(error: &age::DecryptError) -> &'static str {
    match error {
        age::DecryptError::Plugin(_) => {
            "Age plugin failed: check device connection, PIN, confirmation and plugin version"
        }
        age::DecryptError::PluginResolve(_) => {
            "Age plugin unavailable: install the required age-plugin executable on PATH"
        }
        _ => {
            "identity did not unlock this envelope (wrong credential, denied interaction or damaged envelope)"
        }
    }
}

pub(crate) struct Identities {
    pub entries: Vec<Identity>,
    pub failures: Vec<String>,
}

pub(crate) fn identities<C: Callbacks>(path: &Path, callbacks: C) -> Result<Identities> {
    ensure!(
        path != Path::new("-"),
        "identity inputs require a file path"
    );
    let bytes = bounded_read(
        File::open(path).context("could not open Age identity file")?,
        INPUT_LIMIT,
    )?;
    if encrypted_identity(&bytes) {
        // An unused encrypted file must not prompt before a supplied software key can succeed.
        return Ok(Identities {
            entries: vec![Identity::Deferred {
                initialize: Some(Box::new(move || {
                    let loaded = parse_identities(&bytes, callbacks.clone())
                    .map_err(|_| anyhow::anyhow!("encrypted identity unavailable: check identity-file password, format and permissions"))?;
                    for failure in &loaded.failures {
                        callbacks.display_message(failure);
                    }
                    Ok(loaded)
                })),
                loaded: None,
            }],
            failures: Vec::new(),
        });
    }
    parse_identities(&bytes, callbacks)
}

fn encrypted_identity(bytes: &[u8]) -> bool {
    bytes.starts_with(b"age-encryption.org/")
        || bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----")
}

fn parse_identities<C: Callbacks>(bytes: &[u8], callbacks: C) -> Result<Identities> {
    let mut plaintext;
    let bytes = if encrypted_identity(bytes) {
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(bytes))?;
        ensure!(
            decryptor.is_scrypt(),
            "encrypted identity files must be passphrase-protected"
        );
        let password = callbacks
            .request_passphrase("Age identity file passphrase")
            .context("identity-file password input unavailable or cancelled")?;
        let identity = age::scrypt::Identity::new(password);
        let reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|_| anyhow::anyhow!("could not decrypt Age identity file"))?;
        // Authenticate the complete outer file before using any embedded identity or plugin.
        plaintext = bounded_read(reader, INPUT_LIMIT)?;
        plaintext.as_mut_slice()
    } else {
        bytes
    };
    if bytes.starts_with(b"-----BEGIN") {
        let identity = age::ssh::Identity::from_buffer(std::io::Cursor::new(bytes), None)
            .map_err(|_| anyhow::anyhow!("invalid or unsupported SSH identity file"))?;
        return Ok(Identities {
            entries: vec![Identity::Library(Box::new(
                identity.with_callbacks(callbacks),
            ))],
            failures: Vec::new(),
        });
    }
    let text = std::str::from_utf8(bytes).context("Age identity file is not UTF-8")?;
    let mut result = Identities {
        entries: Vec::new(),
        failures: Vec::new(),
    };
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let entry = if line.starts_with("AGE-SECRET-KEY-PQ-1") {
            // A malformed entry must not discard recovery identities from the same file.
            if line.len() <= 1024 {
                Ok(Identity::NativePq(Zeroizing::new(format!("{line}\n"))))
            } else {
                Err(anyhow::anyhow!("native PQ identity exceeds size limit"))
            }
        } else if let Ok(identity) = line.parse::<age::plugin::Identity>() {
            age::plugin::IdentityPluginV1::new(
                identity.plugin(),
                std::slice::from_ref(&identity),
                callbacks.clone(),
            )
            .map(|plugin| Identity::Library(Box::new(plugin)))
            .map_err(|_| anyhow::anyhow!("install age-plugin-{} on PATH", identity.plugin()))
        } else {
            // Per-line conversion prevents one missing plugin poisoning a whole identity file.
            age::IdentityFile::from_buffer(std::io::Cursor::new(line.as_bytes()))
                .map_err(anyhow::Error::from)
                .and_then(|file| {
                    file.with_callbacks(callbacks.clone())
                        .into_identities()
                        .map_err(anyhow::Error::from)
                })
                .and_then(|entries| entries.into_iter().next().context("empty identity entry"))
                .map(|entry| Identity::Library(entry))
        };
        match entry {
            Ok(entry) => result.entries.push(entry),
            Err(error) if result.failures.len() < 32 => {
                // Only public plugin names and our own fixed parser error are safe to display.
                let reason = if line.starts_with("AGE-PLUGIN-") {
                    error.to_string()
                } else {
                    "invalid or unsupported Age identity".to_owned()
                };
                result
                    .failures
                    .push(format!("line {}: {reason}", number + 1));
            }
            Err(_) => (),
        }
    }
    ensure!(
        !result.entries.is_empty() || !result.failures.is_empty(),
        "Age identity file contains no identities"
    );
    Ok(result)
}

/// The published Rust API lacks native software-PQ identities; delegate only this gap.
fn cli(arguments: &[&str], input: &[u8], limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let version = run_cli(&["--version"], &[], 256)?;
    let version = std::str::from_utf8(&version)
        .context("invalid Age version output")?
        .trim();
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    ensure!(
        (major, minor) >= (1, 3),
        "native PQ requires standard age >= 1.3 on PATH"
    );
    run_cli(arguments, input, limit)
}

fn run_cli(arguments: &[&str], input: &[u8], limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let mut child = Command::new("age")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("native PQ requires standard age >= 1.3 on PATH")?;
    let result = (|| {
        child
            .stdin
            .take()
            .context("Age stdin unavailable")?
            .write_all(input)?;
        let output = bounded_read(
            child.stdout.take().context("Age stdout unavailable")?,
            limit as u64,
        )?;
        ensure!(
            child.wait()?.success(),
            "native PQ operation failed; check age >= 1.3 and key compatibility"
        );
        Ok(output)
    })();
    if result.is_err() {
        // Reap children even when an invalid client exceeds the bounded output contract.
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use age::Callbacks;
    use age::secrecy::{ExposeSecret, SecretString};
    use rstest::rstest;
    use zeroize::Zeroizing;

    use super::{Identity, bounded_read, parse_identities, wrap};

    #[derive(Clone)]
    struct Password;

    impl Callbacks for Password {
        fn display_message(&self, _: &str) {}
        fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
            None
        }
        fn request_public_string(&self, _: &str) -> Option<String> {
            None
        }
        fn request_passphrase(&self, _: &str) -> Option<SecretString> {
            Some("password".into())
        }
    }

    #[rstest]
    #[case(false, false)]
    #[case(false, true)]
    #[case(true, false)]
    #[case(true, true)]
    fn an_oversized_pq_entry_cannot_discard_an_independent_recovery_identity(
        #[case] oversized_first: bool,
        #[case] encrypted: bool,
    ) {
        let identity = age::x25519::Identity::generate();
        let oversized = format!("AGE-SECRET-KEY-PQ-1{}", "Q".repeat(1024));
        let secret = identity.to_string();
        let text = Zeroizing::new(if oversized_first {
            format!("{oversized}\n{}\n", secret.expose_secret())
        } else {
            format!("{}\n{oversized}\n", secret.expose_secret())
        });
        let mut ciphertext = Vec::new();
        let input = if encrypted {
            let mut password = age::scrypt::Recipient::new("password".into());
            password.set_work_factor(1);
            let mut writer =
                age::Encryptor::with_recipients(std::iter::once(&password as &dyn age::Recipient))
                    .unwrap()
                    .wrap_output(&mut ciphertext)
                    .unwrap();
            writer.write_all(text.as_bytes()).unwrap();
            writer.finish().unwrap();
            ciphertext.as_slice()
        } else {
            text.as_bytes()
        };
        let mut loaded = parse_identities(input, Password).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.failures.len(), 1);
        let (_, envelope) = wrap(
            &[7; 96],
            &identity.to_public().to_string(),
            age::NoCallbacks,
        )
        .unwrap();
        assert_eq!(
            loaded.entries[0]
                .decrypt(&envelope)
                .unwrap()
                .unwrap()
                .as_slice(),
            &[7; 96]
        );
    }

    #[rstest]
    #[case(false, false)]
    #[case(false, true)]
    #[case(true, false)]
    #[case(true, true)]
    fn encrypted_identity_files_keep_usable_identities_when_a_plugin_is_missing(
        #[case] armored: bool,
        #[case] plugin_first: bool,
    ) {
        let identity = age::x25519::Identity::generate();
        let missing = age::plugin::Identity::default_for_plugin("overmissingfixture").unwrap();
        let software = identity.to_string();
        let text = if plugin_first {
            format!("# Passage\r\n{missing}\r\n{}\r\n", software.expose_secret())
        } else {
            format!("{}\n{missing}\n", software.expose_secret())
        };
        let mut password = age::scrypt::Recipient::new("password".into());
        password.set_work_factor(1);
        let mut encrypted = Vec::new();
        let mut writer =
            age::Encryptor::with_recipients(std::iter::once(&password as &dyn age::Recipient))
                .unwrap()
                .wrap_output(&mut encrypted)
                .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
        writer.finish().unwrap();
        if armored {
            let mut encoded = Vec::new();
            let mut writer = age::armor::ArmoredWriter::wrap_output(
                &mut encoded,
                age::armor::Format::AsciiArmor,
            )
            .unwrap();
            writer.write_all(&encrypted).unwrap();
            writer.finish().unwrap();
            encrypted = encoded;
        }
        let mut loaded = parse_identities(&encrypted, Password).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.failures.len(), 1);
        let (_, envelope) = wrap(
            &[7; 96],
            &identity.to_public().to_string(),
            age::NoCallbacks,
        )
        .unwrap();
        assert_eq!(
            loaded.entries[0]
                .decrypt(&envelope)
                .unwrap()
                .unwrap()
                .as_slice(),
            &[7; 96]
        );
        encrypted.truncate(encrypted.len() / 2);
        assert!(parse_identities(&encrypted, Password).is_err());
    }

    #[test]
    fn bounded_key_reads_reject_extra_bytes_and_truncated_authenticated_streams() {
        assert!(bounded_read(&[0; 97][..], 96).is_err());
        let identity = age::x25519::Identity::generate();
        let (_, mut envelope) = wrap(
            &[7; 96],
            &identity.to_public().to_string(),
            age::NoCallbacks,
        )
        .unwrap();
        envelope.pop();
        assert!(
            Identity::Library(Box::new(identity))
                .decrypt(&envelope)
                .is_err()
        );
    }

    #[test]
    fn invalid_identity_diagnostics_never_contain_the_source_line() {
        let loaded = parse_identities(b"private-input-sentinel\n", age::NoCallbacks).unwrap();
        assert!(loaded.entries.is_empty());
        assert!(
            !loaded
                .failures
                .join("\n")
                .contains("private-input-sentinel")
        );
    }
}
