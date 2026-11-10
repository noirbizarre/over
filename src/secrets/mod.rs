//! Repository-scoped secrets and the shared Age passphrase lifecycle.

pub(crate) mod crypto;
mod file;
mod store;

pub use crypto::{CryptoService, SecretBytes};
pub(crate) use file::validate_ingestion;
pub use file::{EncryptedFile, is_encrypted, reject_encrypted_directory};
pub use store::{SecretName, SecretStore};
