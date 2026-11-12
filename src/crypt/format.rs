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
        ensure!(domain <= 1, "unsupported Over ciphertext domain");
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
    inspect_len(bytes.len())?;
    ensure!(bytes[8] == 1, "unsupported Over ciphertext version");
    ensure!(bytes[9] <= 1, "unsupported Over ciphertext domain");
    Ok(())
}

fn inspect_len(length: usize) -> Result<()> {
    ensure!(length >= HEADER_LEN + 16, "truncated Over ciphertext");
    ensure!(
        length <= MAX_BYTES + HEADER_LEN + 16,
        "protected content exceeds 256 MiB limit"
    );
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
    fn structural_length_bounds_include_exactly_the_header_tag_and_plaintext_limit() {
        for length in 0..HEADER_LEN + 16 {
            assert!(inspect_len(length).is_err());
        }
        for length in [HEADER_LEN + 16, MAX_BYTES + HEADER_LEN + 16] {
            assert!(inspect_len(length).is_ok());
        }
        for length in [MAX_BYTES + HEADER_LEN + 17, usize::MAX] {
            assert!(inspect_len(length).is_err());
        }
    }

    #[test]
    fn encryption_rejects_every_unsupported_domain() {
        let key = RepositoryKey::decode(&[42; 96]).unwrap();
        for domain in 2..=u8::MAX {
            assert!(key.encrypt(b"secret", domain).is_err(), "domain {domain}");
        }
    }

    #[test]
    fn production_aes256_wire_bytes_match_an_independent_known_answer() {
        let mut encoded = [0; 96];
        encoded[16..32].copy_from_slice(&std::array::from_fn::<_, 16, _>(|i| i as u8 + 16));
        encoded[32..].copy_from_slice(&std::array::from_fn::<_, 64, _>(|i| i as u8));
        let key = RepositoryKey::decode(&encoded).unwrap();
        // Generated independently with PyCryptodome AES.MODE_SIV and the full v1 header as AD.
        let expected = "004f5645524352590100101112131415161718191a1b1c1d1e1f6a75a81bd693f9ba7fea267862df0a7fbb913381480ed95de1";
        let encrypted = key.encrypt(b"secret\0\xff\n", 0).unwrap();
        let actual: String = encrypted.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(actual, expected);
        assert_eq!(
            key.decrypt(&encrypted, 0).unwrap().as_slice(),
            b"secret\0\xff\n"
        );
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(1)]
    fn both_domains_authenticate_the_entire_header_tag_and_payload(#[case] domain: u8) {
        let key = RepositoryKey::decode(&[42; 96]).unwrap();
        let encrypted = key.encrypt(b"secret\0\xff\n", domain).unwrap();
        assert_eq!(file_domain(&encrypted).unwrap(), domain);
        assert_eq!(
            key.decrypt(&encrypted, domain).unwrap().as_slice(),
            b"secret\0\xff\n"
        );
        assert!(key.decrypt(&encrypted, domain ^ 1).is_err());
        let mut changed_domain = encrypted.clone();
        changed_domain[9] ^= 1;
        assert_eq!(
            key.decrypt(&changed_domain, domain ^ 1)
                .unwrap_err()
                .to_string(),
            "protected content authentication failed"
        );
        let mut changed_generation = encrypted.clone();
        changed_generation[10] ^= 1;
        let mut matching_metadata_key = RepositoryKey::decode(&[42; 96]).unwrap();
        matching_metadata_key.generation[0] ^= 1;
        // Matching the edited public metadata must not bypass authentication of that header.
        assert_eq!(
            matching_metadata_key
                .decrypt(&changed_generation, domain)
                .unwrap_err()
                .to_string(),
            "protected content authentication failed"
        );
        for offset in 0..encrypted.len() {
            let mut tampered = encrypted.clone();
            tampered[offset] ^= 1;
            assert!(key.decrypt(&tampered, domain).is_err(), "offset {offset}");
        }
        let mut wrong_secret = [42; 96];
        wrong_secret[32] ^= 1;
        // Keep public metadata identical so this must reach MAC verification.
        assert_eq!(
            RepositoryKey::decode(&wrong_secret)
                .unwrap()
                .decrypt(&encrypted, domain)
                .unwrap_err()
                .to_string(),
            "protected content authentication failed"
        );
        let mut wrong_generation = [42; 96];
        wrong_generation[16] ^= 1;
        assert!(
            RepositoryKey::decode(&wrong_generation)
                .unwrap()
                .decrypt(&encrypted, domain)
                .unwrap_err()
                .to_string()
                .contains("different key generation")
        );
    }

    #[test]
    fn inspection_rejects_all_truncations_unsupported_versions_and_domains() {
        let key = RepositoryKey::decode(&[42; 96]).unwrap();
        let encrypted = key.encrypt(b"", 0).unwrap();
        for length in 0..encrypted.len() {
            assert!(inspect(&encrypted[..length]).is_err(), "length {length}");
        }
        for version in (0..=u8::MAX).filter(|version| *version != 1) {
            let mut invalid = encrypted.clone();
            invalid[8] = version;
            assert!(inspect(&invalid).is_err(), "version {version}");
        }
        for domain in 2..=u8::MAX {
            let mut invalid = encrypted.clone();
            invalid[9] = domain;
            assert!(inspect(&invalid).is_err(), "domain {domain}");
        }
        for length in 1..=MAGIC.len() {
            assert!(recognized(&MAGIC[..length]));
        }
        assert!(!recognized(b""));
        assert!(!recognized(b"ordinary\0\xff"));
    }

    #[test]
    fn repository_key_records_require_exactly_the_metadata_and_aes256_key_bytes() {
        for length in (0..=97).filter(|length| *length != 96) {
            assert!(
                RepositoryKey::decode(&vec![42; length]).is_err(),
                "length {length}"
            );
        }
        let key = RepositoryKey::decode(&[42; 96]).unwrap();
        assert_eq!(key.encode().as_slice(), &[42; 96]);
        assert_eq!(format!("{key:?}"), "RepositoryKey([REDACTED])");
    }

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
