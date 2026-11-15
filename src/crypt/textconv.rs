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
    let _guard = access::guard(repo, false)?;
    let domain = format::file_domain(input)?;
    // Unlike smudge, conversion must neither pass locked ciphertext through nor
    // keep the native-store domain encrypted: this boundary is explicit display.
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
    fn recognizable_truncated_ciphertext_never_becomes_displayable_plaintext() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let ciphertext = format::RepositoryKey::generate()
            .unwrap()
            .encrypt(b"secret", 0)
            .unwrap();
        for end in 1..ciphertext.len() {
            assert!(transform(&repo, &ciphertext[..end]).is_err());
        }
    }
}
