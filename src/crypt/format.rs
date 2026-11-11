//! Version 1 uses RFC 5297 AES-256-SIV, not nonce-sensitive fixed-nonce AEAD.

use aes_siv::{KeyInit, siv::Aes256Siv};
use anyhow::{Result, bail, ensure};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 8] = b"\0OVERCRY";
pub const HEADER_LEN: usize = 26;
pub const MAX_BYTES: usize = 256 * 1024 * 1024;

/// The generation is random public metadata, never a hash of the secret key.
pub struct RepositoryKey {
    pub repository: [u8; 16],
    pub generation: [u8; 16],
    bytes: Zeroizing<[u8; 64]>,
}

impl std::fmt::Debug for RepositoryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RepositoryKey([REDACTED])")
    }
}

impl RepositoryKey {
    pub fn generate() -> Result<Self> {
        let mut bytes = Zeroizing::new([0; 64]);
        let mut generation = [0; 16];
        getrandom::fill(bytes.as_mut())
            .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
        getrandom::fill(&mut generation)
            .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
        let mut repository = [0; 16];
        getrandom::fill(&mut repository)
            .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
        Ok(Self {
            repository,
            generation,
            bytes,
        })
    }

    pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut output = Zeroizing::new(self.repository.to_vec());
        output.extend_from_slice(&self.generation);
        output.extend_from_slice(self.bytes.as_ref());
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() == 96, "invalid repository key");
        let mut secret = Zeroizing::new([0; 64]);
        secret.copy_from_slice(&bytes[32..]);
        Ok(Self {
            repository: bytes[..16].try_into()?,
            generation: bytes[16..32].try_into()?,
            bytes: secret,
        })
    }

    pub fn encrypt(&self, plaintext: &[u8], domain: u8) -> Result<Vec<u8>> {
        ensure!(
            plaintext.len() <= MAX_BYTES,
            "protected content exceeds 256 MiB limit"
        );
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&[1, domain]);
        header.extend_from_slice(&self.generation);
        let payload = Aes256Siv::new_from_slice(self.bytes.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid repository key"))?
            .encrypt([header.as_slice()], plaintext)
            .map_err(|_| anyhow::anyhow!("could not encrypt protected content"))?;
        header.extend_from_slice(&payload);
        Ok(header)
    }

    pub fn decrypt(&self, ciphertext: &[u8], domain: u8) -> Result<Zeroizing<Vec<u8>>> {
        inspect(ciphertext)?;
        ensure!(
            ciphertext[9] == domain,
            "protected content has the wrong domain"
        );
        ensure!(
            ciphertext[10..HEADER_LEN] == self.generation,
            "protected content uses a different key generation"
        );
        // SIV verifies the whole message before returning any plaintext to callers.
        Aes256Siv::new_from_slice(self.bytes.as_ref())
            .map_err(|_| anyhow::anyhow!("invalid repository key"))?
            .decrypt([&ciphertext[..HEADER_LEN]], &ciphertext[HEADER_LEN..])
            .map(Zeroizing::new)
            .map_err(|_| anyhow::anyhow!("protected content authentication failed"))
    }
}

/// Structural validation is not authentication; doctor reports that distinction.
pub fn inspect(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.starts_with(MAGIC),
        "protected blob is plaintext, not Over ciphertext"
    );
    ensure!(bytes.len() >= HEADER_LEN + 16, "truncated Over ciphertext");
    ensure!(
        bytes.len() <= MAX_BYTES + HEADER_LEN + 16,
        "protected content exceeds 256 MiB limit"
    );
    ensure!(bytes[8] == 1, "unsupported Over ciphertext version");
    ensure!(bytes[9] <= 1, "unsupported Over ciphertext domain");
    Ok(())
}

pub fn recognized(bytes: &[u8]) -> bool {
    // A truncated magic prefix must not be encrypted again as ordinary content.
    bytes.starts_with(MAGIC) || (!bytes.is_empty() && MAGIC.starts_with(bytes))
}

pub fn file_domain(bytes: &[u8]) -> Result<u8> {
    inspect(bytes)?;
    match bytes[9] {
        domain @ (0 | 1) => Ok(domain),
        _ => bail!("unsupported protected content domain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_siv_matches_the_published_rfc_5297_vector() {
        fn hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }
        let key = hex("fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let ad = hex("101112131415161718191a1b1c1d1e1f2021222324252627");
        let plaintext = hex("112233445566778899aabbccddee");
        let output = aes_siv::siv::Aes128Siv::new_from_slice(&key)
            .unwrap()
            .encrypt([ad.as_slice()], &plaintext)
            .unwrap();
        assert_eq!(
            output,
            hex("85632d07c6e8f37f950acd320a2ecc9340c02b9690c4dc04daef7f6afe5c")
        );
    }

    #[test]
    fn unchanged_binary_content_has_identical_authenticated_ciphertext() {
        let key = RepositoryKey::decode(&[42; 96]).unwrap();
        for plaintext in [b"".as_slice(), b"secret\0\xff\n".as_slice()] {
            let encrypted = key.encrypt(plaintext, 0).unwrap();
            assert_eq!(encrypted, key.encrypt(plaintext, 0).unwrap());
            assert_eq!(key.decrypt(&encrypted, 0).unwrap().as_slice(), plaintext);
            let mut tampered = encrypted.clone();
            *tampered.last_mut().unwrap() ^= 1;
            assert!(key.decrypt(&tampered, 0).is_err());
            assert!(key.decrypt(&encrypted, 1).is_err());
            assert!(
                RepositoryKey::decode(&[43; 96])
                    .unwrap()
                    .decrypt(&encrypted, 0)
                    .is_err()
            );
        }
    }
}
