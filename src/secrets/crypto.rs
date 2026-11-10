use std::fmt;
use std::io::{IsTerminal, Read, Write};
use std::sync::Mutex;

use age::secrecy::SecretString;
use anyhow::{Result, anyhow, bail};
use zeroize::Zeroizing;

/// Plaintext never implements a value-bearing Debug or serialization.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}

type Source = dyn Fn(bool) -> Result<Option<SecretString>> + Send + Sync;

/// One invocation's lazy, shared credential capability; plaintext is not cached.
pub struct CryptoService {
    credential: Mutex<Option<SecretString>>,
    source: Box<Source>,
}

impl fmt::Debug for CryptoService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CryptoService([REDACTED])")
    }
}

impl Default for CryptoService {
    fn default() -> Self {
        Self::from_sources(
            || match std::env::var("OVER_SECRET") {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotUnicode(_)) => {
                    bail!("OVER_SECRET must contain valid Unicode")
                }
                Err(std::env::VarError::NotPresent) => Ok(None),
            },
            || hidden_input("Age passphrase").map(Into::into),
        )
    }
}

impl CryptoService {
    fn from_sources(
        environment: impl Fn() -> Result<Option<String>> + Send + Sync + 'static,
        prompt: impl Fn() -> Result<SecretString> + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(move |interactive| {
            if let Some(value) = environment()?.filter(|value| !value.is_empty()) {
                return Ok(Some(value.into()));
            }
            if interactive {
                prompt().map(Some)
            } else {
                Ok(None)
            }
        })
    }

    fn with_source(
        source: impl Fn(bool) -> Result<Option<SecretString>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            credential: Mutex::new(None),
            source: Box::new(source),
        }
    }

    fn credential(&self, prompt: bool) -> Result<Option<SecretString>> {
        // Serializing acquisition prevents parallel consumers from prompting twice.
        let mut cached = self
            .credential
            .lock()
            .map_err(|_| anyhow!("secret service unavailable"))?;
        if cached.is_none() {
            *cached = (self.source)(prompt)?;
        }
        Ok(cached.clone())
    }

    pub fn available(&self) -> Result<bool> {
        Ok(self.credential(false)?.is_some())
    }

    pub fn encrypt(&self, plaintext: &[u8], prompt: bool) -> Result<Vec<u8>> {
        let credential = self.credential(prompt)?.ok_or_else(locked)?;
        let encryptor = age::Encryptor::with_user_passphrase(credential);
        let mut ciphertext = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut ciphertext)
            .map_err(|_| anyhow!("could not initialize Age encryption"))?;
        writer
            .write_all(plaintext)
            .map_err(|_| anyhow!("could not encrypt secret"))?;
        writer
            .finish()
            .map_err(|_| anyhow!("could not finish Age encryption"))?;
        Ok(ciphertext)
    }

    pub fn decrypt(&self, ciphertext: &[u8], prompt: bool) -> Result<SecretBytes> {
        // Reject malformed input before requesting a credential it cannot use.
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(ciphertext))
            .map_err(|_| anyhow!("invalid Age ciphertext"))?;
        let credential = self.credential(prompt)?.ok_or_else(locked)?;
        let identity = age::scrypt::Identity::new(credential);
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|_| {
                anyhow!("could not decrypt Age data; check OVER_SECRET or the passphrase")
            })?;
        let mut plaintext = SecretBytes::new(Vec::new());
        // Authenticate EOF before exposing any bytes to a destination.
        reader
            .read_to_end(&mut plaintext.0)
            .map_err(|_| anyhow!("Age data is corrupt or incomplete"))?;
        Ok(plaintext)
    }
}

fn locked() -> anyhow::Error {
    anyhow!("Age data is locked; set OVER_SECRET or enable interactive prompting")
}

pub(crate) fn hidden_input(label: &str) -> Result<String> {
    // Stdin belongs to piped secret values, never to passphrase acquisition.
    if !std::io::stderr().is_terminal() {
        bail!(
            "cannot prompt securely; set OVER_SECRET and supply the secret value on stdin when adding a secret"
        );
    }
    dialoguer::Password::new().with_prompt(label).interact()
        .map_err(|_| anyhow!("secure input unavailable; set OVER_SECRET and supply the secret value on stdin when adding a secret"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn service(passphrase: &str) -> CryptoService {
        let passphrase = passphrase.to_owned();
        CryptoService::with_source(move |_| Ok(Some(passphrase.clone().into())))
    }

    #[test]
    fn binary_secrets_round_trip_without_disclosing_debug_values() {
        let crypto = service("passphrase sentinel");
        let ciphertext = crypto.encrypt(b"value sentinel\0\n", false).unwrap();
        let plaintext = crypto.decrypt(&ciphertext, false).unwrap();
        assert_eq!(plaintext.expose(), b"value sentinel\0\n");
        assert!(!format!("{crypto:?} {plaintext:?}").contains("sentinel"));
        let error = service("wrong").decrypt(&ciphertext, false).unwrap_err();
        assert!(!format!("{error:#}").contains("sentinel"));
        assert!(
            crypto
                .decrypt(&ciphertext[..ciphertext.len() - 1], false)
                .is_err()
        );
    }

    #[test]
    fn multiple_resolutions_acquire_the_credential_only_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let crypto = CryptoService::with_source(move |prompt| {
            if !prompt {
                return Ok(None);
            }
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Some("passphrase".into()))
        });
        assert!(!crypto.available().unwrap());
        let first = crypto.encrypt(b"first", true).unwrap();
        let second = crypto.encrypt(b"second", true).unwrap();
        crypto.decrypt(&first, false).unwrap();
        crypto.decrypt(&second, false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[rstest::rstest]
    #[case(None, true)]
    #[case(Some(""), true)]
    #[case(Some("environment"), false)]
    #[case(Some(" "), false)]
    fn environment_precedence_and_empty_fallback_share_one_policy(
        #[case] value: Option<&'static str>,
        #[case] prompts: bool,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let crypto = CryptoService::from_sources(
            move || Ok(value.map(str::to_owned)),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok("interactive".into())
            },
        );
        assert_eq!(crypto.available().unwrap(), !prompts);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        crypto.credential(true).unwrap();
        crypto.credential(true).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(prompts));
    }

    #[test]
    fn context_copies_store_resolutions_and_files_share_one_unlock() {
        use crate::secrets::{EncryptedFile, SecretName, SecretStore};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let crypto = Arc::new(CryptoService::from_sources(
            || Ok(None),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok("passphrase".into())
            },
        ));
        let context = crate::exec::Context {
            crypto: crypto.clone(),
            ..Default::default()
        };
        let copy = context.with_progress(indicatif::ProgressBar::hidden());
        assert!(Arc::ptr_eq(&copy.crypto, &crypto));
        let temp = tempfile::TempDir::new().unwrap();
        let store = SecretStore::new(temp.path(), crypto.clone()).unwrap();
        let name = SecretName::parse("one").unwrap();
        store
            .add(&name, &SecretBytes::new(b"first".to_vec()), false, true)
            .unwrap();
        store.get(&name, true).unwrap();
        let file = EncryptedFile {
            source: temp.path().join(".over/secrets/one.age"),
            crypto: copy.crypto.clone(),
        };
        file.read(true).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    crypto.credential(true).unwrap();
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
