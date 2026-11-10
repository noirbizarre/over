use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use super::{CryptoService, SecretBytes, SecretReference, SecretStore};

/// Only explicitly recoverable failures may advance a fallback chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderError {
    Missing,
    Unavailable,
    SessionUnavailable,
    Failed,
    StoreFailure,
    DecryptionFailed,
    InvalidReference,
}

impl ProviderError {
    pub fn is_recoverable(self) -> bool {
        matches!(
            self,
            Self::Missing | Self::Unavailable | Self::SessionUnavailable
        )
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "secret does not exist",
            Self::Unavailable => "secret provider is unavailable",
            Self::SessionUnavailable => "secret provider session is unavailable",
            Self::Failed => "secret provider failed",
            Self::StoreFailure => "could not read native secret store",
            Self::DecryptionFailed => "could not decrypt native secret; check OVER_SECRET or the passphrase and ciphertext",
            Self::InvalidReference => "provider received an invalid secret reference",
        })
    }
}
impl std::error::Error for ProviderError {}

/// Implementations must return safe categories, never plaintext-bearing errors.
pub trait SecretProvider: Send + Sync {
    fn resolve(&self, reference: &SecretReference) -> Result<SecretBytes, ProviderError>;
}

pub struct NativeProvider {
    repository: PathBuf,
    crypto: Arc<CryptoService>,
    prompt: bool,
}

impl NativeProvider {
    pub fn new(repository: PathBuf, crypto: Arc<CryptoService>, prompt: bool) -> Self {
        Self {
            repository,
            crypto,
            prompt,
        }
    }
}

impl SecretProvider for NativeProvider {
    fn resolve(&self, reference: &SecretReference) -> Result<SecretBytes, ProviderError> {
        let SecretReference::Native(name) = reference else {
            return Err(ProviderError::InvalidReference);
        };
        // Delay all filesystem and credential work until this candidate is reached.
        let store = SecretStore::new(&self.repository, self.crypto.clone())
            .map_err(|_| ProviderError::StoreFailure)?;
        store.read_for_resolution(name, self.prompt)
    }
}
