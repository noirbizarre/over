//! Repository-scoped secrets and the shared Age passphrase lifecycle.

mod commit;
mod config;
pub(crate) mod crypto;
pub(crate) mod entry;
mod file;
pub(crate) mod layout;
mod provider;
mod reference;
mod resolver;
mod snapshot;
mod store;

pub use config::{CommitMessages, SecretOperation, SecretsConfig};
pub use crypto::{CryptoService, SecretBytes};
pub(crate) use file::validate_ingestion;
pub use file::{EncryptedFile, is_encrypted, reject_encrypted_directory};
pub use provider::{NativeProvider, ProviderError, SecretProvider};
pub use reference::{InvalidReference, SecretChain, SecretReference};
pub use resolver::{ResolutionError, SecretResolver};
pub(crate) use store::migrate_native;
pub use store::{SecretName, SecretStore};
