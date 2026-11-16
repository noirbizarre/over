//! Independently authenticated native entries, with filename/name agreement.

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize as _;

#[derive(Deserialize, Serialize)]
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

pub(super) fn encode(
    key: &crate::crypt::format::RepositoryKey,
    name: &super::SecretName,
    bytes: &[u8],
) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Value<'a> {
        version: u32,
        name: &'a str,
        value: &'a [u8],
    }
    key.encrypt(
        &crate::crypt::buffer::to_json(&Value {
            version: 1,
            name: name.as_str(),
            value: bytes,
        })?,
        1,
    )
}

pub(super) fn decode(
    key: &crate::crypt::format::RepositoryKey,
    name: &super::SecretName,
    bytes: &[u8],
) -> Result<super::SecretBytes> {
    let plaintext = key.decrypt(bytes, 1)?;
    let mut entry: Entry =
        serde_json::from_slice(&plaintext).context("invalid native secret entry")?;
    ensure!(
        entry.version == 1 && entry.name == name.as_str(),
        "unsupported or name-mismatched native secret entry"
    );
    Ok(super::SecretBytes::new(std::mem::take(&mut entry.value)))
}
