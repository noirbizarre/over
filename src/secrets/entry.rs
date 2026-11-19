//! Independently authenticated native entries, with filename/name agreement.

use anyhow::{Result, ensure};
use serde::Deserialize;
use zeroize::Zeroize as _;

#[derive(Deserialize)]
struct Entry {
    version: u32,
    name: String,
    value: Vec<u8>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

pub(crate) fn encode(
    key: &crate::crypt::format::RepositoryKey,
    name: &super::SecretName,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    key.encrypt_secret(bytes, name.as_str())
}

pub(crate) fn decode(
    key: &crate::crypt::format::RepositoryKey,
    name: &super::SecretName,
    bytes: &[u8],
) -> Result<super::SecretBytes> {
    if crate::crypt::format::file_domain(bytes)? == 2 {
        return Ok(super::SecretBytes::from_zeroizing(
            key.decrypt_secret(bytes, name.as_str())?,
        ));
    }
    let plaintext = key.decrypt(bytes, 1)?;
    let mut entry: Entry = serde_json::from_slice(&plaintext)
        .map_err(|_| anyhow::anyhow!("invalid native secret entry"))?;
    ensure!(
        entry.version == 1 && entry.name == name.as_str(),
        "unsupported or name-mismatched native secret entry"
    );
    Ok(super::SecretBytes::new(std::mem::take(&mut entry.value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypt::format::{self, RepositoryKey};
    use crate::secrets::SecretName;

    #[test]
    fn raw_entries_preserve_exact_values_and_authenticate_their_names() {
        let key = RepositoryKey::generate().unwrap();
        let name = SecretName::parse("token").unwrap();
        let other = SecretName::parse("Token").unwrap();
        for value in [
            Vec::new(),
            (0..=255).collect(),
            b"{\"version\":1}\n".to_vec(),
            b"\0OVERCRY".to_vec(),
        ] {
            let bytes = encode(&key, &name, &value).unwrap();
            assert_eq!(bytes[9], 2);
            assert_eq!(bytes.len(), value.len() + format::HEADER_LEN + 16);
            assert_eq!(
                key.decrypt_secret(&bytes, name.as_str())
                    .unwrap()
                    .as_slice(),
                value
            );
            assert_eq!(decode(&key, &name, &bytes).unwrap().expose(), value);
            assert_eq!(encode(&key, &name, &value).unwrap(), bytes);
            assert_ne!(encode(&key, &other, &value).unwrap(), bytes);
            assert!(decode(&key, &other, &bytes).is_err());
            assert!(key.decrypt(&bytes, 2).is_err());
            assert!(decode(&RepositoryKey::generate().unwrap(), &name, &bytes).is_err());
            for index in 0..bytes.len() {
                let mut corrupt = bytes.clone();
                corrupt[index] ^= 1;
                assert!(decode(&key, &name, &corrupt).is_err(), "offset {index}");
            }
            for end in 0..bytes.len() {
                assert!(decode(&key, &name, &bytes[..end]).is_err());
            }
        }
    }

    #[test]
    fn legacy_entries_remain_readable_without_confusing_raw_json_values() {
        let key = RepositoryKey::generate().unwrap();
        let name = SecretName::parse("token").unwrap();
        let json = b"{\"version\":1,\"name\":\"token\",\"value\":[0,255,10]}";
        let legacy = key.encrypt(json, 1).unwrap();
        assert_eq!(
            decode(&key, &name, &legacy).unwrap().expose(),
            &[0, 255, 10]
        );
        assert_eq!(
            decode(&key, &name, &encode(&key, &name, json).unwrap())
                .unwrap()
                .expose(),
            json
        );
        for invalid in [
            b"{\"version\":2,\"name\":\"token\",\"value\":[]}".as_slice(),
            b"{\"version\":1,\"name\":\"other\",\"value\":[]}",
            b"{\"sensitive\":\"do-not-print\"}",
        ] {
            let error = decode(&key, &name, &key.encrypt(invalid, 1).unwrap()).unwrap_err();
            assert!(!format!("{error:#}").contains("do-not-print"));
        }
        assert!(decode(&key, &name, &key.encrypt(json, 0).unwrap()).is_err());
    }
}
