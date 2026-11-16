//! Octal permissions for managed partial and generated content; symlink sources remain untouched.

use std::fmt;
use std::num::ParseIntError;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::overlay::Overlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(schema_with = "crate::overlays::json_schema::file_mode")
)]
pub struct FileMode(u32);

/// Names stay declarative until execution so parsing does not depend on local accounts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Identity {
    Id(u32),
    Name(String),
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => id.fmt(f),
            Self::Name(name) => name.fmt(f),
        }
    }
}

/// Each component is an independent assignment in the ordered policy cascade.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(schema_with = "crate::overlays::json_schema::permissions")
)]
pub struct Permissions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<FileMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<Identity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<Identity>,
}

impl fmt::Display for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(mode) = self.mode {
            write!(f, "{mode}")?;
        }
        for (label, identity) in [("owner", &self.owner), ("group", &self.group)] {
            if let Some(identity) = identity {
                match identity {
                    Identity::Id(id) => write!(f, " {label}={id}")?,
                    Identity::Name(name) => write!(f, " {label}={name}")?,
                }
            }
        }
        Ok(())
    }
}

impl Permissions {
    pub fn merge(&mut self, assignment: &Self) {
        if let Some(mode) = assignment.mode {
            self.mode = Some(mode);
        }
        if let Some(owner) = &assignment.owner {
            self.owner = Some(owner.clone());
        }
        if let Some(group) = &assignment.group {
            self.group = Some(group.clone());
        }
    }
}

impl<'de> Deserialize<'de> for Permissions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            mode: Option<FileMode>,
            owner: Option<Identity>,
            group: Option<Identity>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Declaration {
            Mode(FileMode),
            Object(Fields),
        }
        let permissions = match Declaration::deserialize(deserializer)? {
            Declaration::Mode(mode) => Self {
                mode: Some(mode),
                ..Self::default()
            },
            Declaration::Object(fields) => Self {
                mode: fields.mode,
                owner: fields.owner,
                group: fields.group,
            },
        };
        if permissions.mode.is_none() && permissions.owner.is_none() && permissions.group.is_none()
        {
            return Err(serde::de::Error::custom(
                "permissions must assign mode, owner or group",
            ));
        }
        for identity in [&permissions.owner, &permissions.group]
            .into_iter()
            .flatten()
        {
            let valid = match identity {
                Identity::Id(id) => *id != u32::MAX,
                Identity::Name(name) => {
                    !name.trim().is_empty()
                        && !name.contains('\0')
                        && (!name.bytes().all(|byte| byte.is_ascii_digit())
                            || name.parse::<u32>().is_ok_and(|id| id != u32::MAX))
                }
            };
            if !valid {
                return Err(serde::de::Error::custom(
                    "invalid permissions owner or group",
                ));
            }
        }
        Ok(permissions)
    }
}

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
