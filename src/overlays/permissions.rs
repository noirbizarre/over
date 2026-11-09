//! Octal permissions for managed partial content; symlink sources remain untouched.

use std::fmt;
use std::num::ParseIntError;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::overlay::Overlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    pub fn parse(s: &str) -> Result<Self, ParseIntError> {
        let value = u32::from_str_radix(s.trim_start_matches("0o"), 8)?;
        // Keep file-type bits out of permissions passed to the filesystem.
        Ok(Self(value & 0o7777))
    }

    pub fn from_bits(bits: u32) -> Self {
        Self(bits & 0o7777)
    }

    pub fn bits(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for FileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:o}", self.0)
    }
}

impl Serialize for FileMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for FileMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

pub(super) fn resolve(overlay: &Overlay, relative: &Path) -> Option<FileMode> {
    super::rules::resolve_properties(overlay, relative, false).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("644")]
    #[case("0644")]
    #[case("0o644")]
    fn octal_permissions_accept_supported_prefixes(#[case] input: &str) {
        assert_eq!(FileMode::parse(input).unwrap().bits(), 0o644);
    }

    #[test]
    fn file_type_bits_cannot_escape_into_permissions() {
        assert_eq!(FileMode::from_bits(0o100644).bits(), 0o644);
        assert!(FileMode::parse("999").is_err());
        assert_eq!(FileMode::parse("0755").unwrap().to_string(), "755");
    }
}
