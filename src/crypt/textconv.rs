//! Authenticate complete Git inputs before exposing plaintext to diff presentation.

use std::io::Write;
use std::path::Path;

use anyhow::{Result, ensure};
use zeroize::Zeroizing;

use super::{access, buffer, format};

pub fn transform(repo: &git2::Repository, input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if !format::recognized(input) {
        ensure!(
            input.len() <= format::MAX_BYTES,
            "protected content exceeds size limit"
        );
        // Git also converts unlocked working files and historical plaintext blobs.
        return Ok(Zeroizing::new(input.to_vec()));
    }
    let domain = format::file_domain(input)?;
    // Checkout can visit protected blobs before their access metadata is present.
    if access::local_key_missing(repo)? {
        return Ok(Zeroizing::new(input.to_vec()));
    }
    // Opaque locked output needs no administrative storage; only decryption
    // requires synchronization with key lifecycle operations.
    let _guard = access::inspection_guard(repo)?;
    // Explicit display decrypts the native store too, but never bypasses authentication.
    access::local_key(repo)?.decrypt(input, domain)
}

pub fn run(path: &Path) -> Result<()> {
    // Git's temporary blob may live outside the repository being compared.
    let repo = git2::Repository::open_from_env().or_else(|_| git2::Repository::discover("."))?;
    let input =
        buffer::read_file_with_limit(path, Some(format::MAX_BYTES + format::HEADER_LEN + 16))?;
    let output = transform(&repo, &input)?;
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_conversion_preserves_exact_bytes_without_unlocking() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        for input in [b"".as_slice(), b"historical\n", b"binary\0\xff"] {
            assert_eq!(transform(&repo, input).unwrap().as_slice(), input);
        }
    }

    #[test]
    fn locked_conversion_rejects_truncated_headers_and_authentication_tags() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let ciphertext = format::RepositoryKey::generate()
            .unwrap()
            .encrypt(b"secret", 0)
            .unwrap();
        for end in 1..format::HEADER_LEN + 16 {
            assert!(transform(&repo, &ciphertext[..end]).is_err());
        }
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(1)]
    fn locked_conversion_preserves_both_domains_without_checked_out_metadata(#[case] domain: u8) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let ciphertext = format::RepositoryKey::generate()
            .unwrap()
            .encrypt(b"binary\0secret\xff\n", domain)
            .unwrap();
        assert_eq!(
            transform(&repo, &ciphertext).unwrap().as_slice(),
            ciphertext
        );
        assert!(!access::private_dir(&repo).exists());
    }
}
