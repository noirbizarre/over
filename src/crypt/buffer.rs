//! Wipe application-owned plaintext allocations, including allocations retired during growth.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Result, ensure};
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

pub(crate) fn reserve_secret(
    bytes: &mut Zeroizing<Vec<u8>>,
    required: usize,
) -> Option<Zeroizing<Vec<u8>>> {
    if required <= bytes.capacity() {
        return None;
    }
    let ceiling = if required <= super::format::MAX_BYTES + super::format::HEADER_LEN + 16 {
        super::format::MAX_BYTES + super::format::HEADER_LEN + 16
    } else {
        usize::MAX
    };
    let capacity = required.max(bytes.capacity().saturating_mul(2).min(ceiling));
    let mut replacement = Zeroizing::new(Vec::with_capacity(capacity));
    replacement.extend_from_slice(bytes);
    let mut retired = std::mem::replace(bytes, replacement);
    retired.as_mut_slice().zeroize();
    Some(retired)
}

#[derive(Default)]
struct PlaintextWriter {
    bytes: Zeroizing<Vec<u8>>,
}

impl Write for PlaintextWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("protected content size overflow"))?;
        if required > super::format::MAX_BYTES {
            return Err(std::io::Error::other(
                "protected content exceeds size limit",
            ));
        }
        drop(reserve_secret(&mut self.bytes, required));
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn to_json(value: &impl Serialize) -> Result<Zeroizing<Vec<u8>>> {
    let mut writer = PlaintextWriter::default();
    // serde_json::to_vec would release unwiped intermediate allocations while growing.
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| anyhow::anyhow!("could not serialize protected content"))?;
    Ok(std::mem::take(&mut writer.bytes))
}

pub(crate) fn read_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    read_file_with_limit(path, None)
}

pub(crate) fn read_file_with_limit(
    path: &Path,
    limit: Option<usize>,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    let size: usize = file.metadata()?.len().try_into()?;
    ensure!(
        limit.is_none_or(|limit| size <= limit),
        "protected content exceeds size limit"
    );
    // No plaintext exists yet, so a single hinted allocation cannot retire a sensitive prefix.
    bytes
        .try_reserve_exact(size)
        .map_err(|_| anyhow::anyhow!("could not allocate file content buffer"))?;
    let mut chunk = Zeroizing::new([0; 8192]);
    loop {
        let length = match file.read(chunk.as_mut()) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            return Ok(bytes);
        }
        let required = bytes.len() + length;
        ensure!(
            limit.is_none_or(|limit| required <= limit),
            "protected content exceeds size limit"
        );
        drop(reserve_secret(&mut bytes, required));
        bytes.extend_from_slice(&chunk[..length]);
    }
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>> {
    match read_file(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_file_reads_do_not_inherit_a_protected_payload_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ordinary");
        std::fs::write(&path, b"ordinary unprotected content").unwrap();
        assert!(read_file_with_limit(&path, Some(8)).is_err());
        assert_eq!(
            read_file(&path).unwrap().as_slice(),
            b"ordinary unprotected content"
        );
    }
}
