//! Strict at-rest protection; unlike smudge filters, reads never return locked ciphertext.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{access, format::RepositoryKey};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    #[default]
    Plain,
    Encrypted,
}

impl Protection {
    pub fn for_path(repo: &git2::Repository, path: &std::path::Path) -> Result<Self> {
        Ok(if super::policy::protected(repo, path)? {
            Self::Encrypted
        } else {
            Self::Plain
        })
    }

    pub fn encode(self, repo: &git2::Repository, plaintext: &[u8]) -> Result<Vec<u8>> {
        if self == Self::Plain {
            return Ok(plaintext.to_vec());
        }
        let _guard = access::guard(repo, false)?;
        self.encode_with_key(plaintext, &access::local_key(repo)?)
    }

    pub fn decode(self, repo: &git2::Repository, stored: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if self == Self::Plain {
            return Ok(Zeroizing::new(stored.to_vec()));
        }
        let _guard = access::guard(repo, false)?;
        self.decode_with_key(stored, &access::local_key(repo)?)
            .context("cannot read protected generated content; unlock the matching repository generation")
    }

    pub(crate) fn encode_with_key(self, plaintext: &[u8], key: &RepositoryKey) -> Result<Vec<u8>> {
        match self {
            Self::Plain => Ok(plaintext.to_vec()),
            Self::Encrypted => key.encrypt(plaintext, 0),
        }
    }

    pub(crate) fn decode_with_key(
        self,
        stored: &[u8],
        key: &RepositoryKey,
    ) -> Result<Zeroizing<Vec<u8>>> {
        match self {
            Self::Plain => Ok(Zeroizing::new(stored.to_vec())),
            Self::Encrypted => key.decrypt(stored, 0),
        }
    }
}
