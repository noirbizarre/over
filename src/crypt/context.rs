//! Adapt logical repository paths to crypto context without coupling the wire format to storage.

use std::path::Path;

use anyhow::Result;
use zeroize::Zeroizing;

use super::format::{self, RepositoryKey};
use crate::secrets::SecretName;

pub(crate) fn secret_name(path: &Path) -> Result<SecretName> {
    let name = path
        .strip_prefix(".over/secrets")
        .ok()
        .filter(|suffix| suffix.components().count() == 1)
        .and_then(|suffix| suffix.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "native secret requires its repository-relative path .over/secrets/NAME; received {}",
                path.display()
            )
        })?;
    SecretName::parse(name)
}

pub(crate) fn decrypt(
    key: &RepositoryKey,
    ciphertext: &[u8],
    path: &Path,
) -> Result<Zeroizing<Vec<u8>>> {
    let domain = format::file_domain(ciphertext)?;
    if domain == 2 {
        // Only the logical repository path can authenticate an entry; blob temp paths cannot.
        key.decrypt_secret(ciphertext, secret_name(path)?.as_str())
    } else {
        key.decrypt(ciphertext, domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_context_requires_a_valid_logical_store_path_with_exact_spelling() {
        let key = RepositoryKey::generate().unwrap();
        let bytes = key.encrypt_secret(b"secret", "Token").unwrap();
        assert_eq!(
            decrypt(&key, &bytes, Path::new(".over/secrets/Token"))
                .unwrap()
                .as_slice(),
            b"secret"
        );
        for invalid in [
            "Token",
            "/tmp/Token",
            ".over/secrets/token",
            ".over/secrets/../Token",
            ".over/secrets/Token/child",
            ".over/secrets/CON",
        ] {
            assert!(
                decrypt(&key, &bytes, Path::new(invalid)).is_err(),
                "{invalid}"
            );
        }
    }
}
