//! Repository-scoped secrets and the shared Age passphrase lifecycle.

mod config;
pub(crate) mod crypto;
mod file;
mod provider;
mod reference;
mod resolver;
mod store;

pub use config::{CommitMessages, SecretOperation, SecretsConfig};
pub use crypto::{CryptoService, SecretBytes};
pub(crate) use file::validate_ingestion;
pub use file::{EncryptedFile, is_encrypted, reject_encrypted_directory};
pub use provider::{NativeProvider, ProviderError, SecretProvider};
pub use reference::{InvalidReference, SecretChain, SecretReference};
pub use resolver::{ResolutionError, SecretResolver};
pub use store::{SecretName, SecretStore};
