use std::fmt;

use super::SecretName;

/// A validated reference; provider payloads are opaque to the dispatcher.
#[derive(Clone)]
pub enum SecretReference {
    Native(SecretName),
    Provider { scheme: String, payload: String },
}

impl SecretReference {
    pub fn parse(input: &str) -> Result<Self, InvalidReference> {
        if let Some((scheme, payload)) = input.split_once("://") {
            // Validate the dispatch envelope without imposing any external provider's path grammar.
            if !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                || !scheme
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
                || payload.is_empty()
                || input
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == '|')
            {
                return Err(InvalidReference);
            }
            Ok(Self::Provider {
                scheme: scheme.to_ascii_lowercase(),
                payload: payload.to_owned(),
            })
        } else {
            SecretName::parse(input)
                .map(Self::Native)
                .map_err(|_| InvalidReference)
        }
    }
}

impl fmt::Debug for SecretReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // References may themselves contain credentials for future providers.
        f.write_str("SecretReference([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidReference;

impl fmt::Display for InvalidReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid secret reference; use a native name or scheme://payload with nonempty pipe-separated candidates")
    }
}
impl std::error::Error for InvalidReference {}

#[derive(Debug, Clone)]
pub struct SecretChain(Vec<SecretReference>);

impl SecretChain {
    pub fn parse(expression: &str) -> Result<Self, InvalidReference> {
        // Validate later candidates too: an early success must not conceal a typo.
        expression
            .split('|')
            .map(|part| SecretReference::parse(part.trim()))
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn candidates(&self) -> &[SecretReference] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("")]
    #[case(" ")]
    #[case("name |")]
    #[case("| name")]
    #[case("name || other")]
    #[case("../name")]
    #[case("op://")]
    #[case("1op://value")]
    #[case("op://has space")]
    #[case("op:value")]
    #[case("op://value\nother")]
    #[case("op://value\0")]
    fn malformed_chains_are_rejected(#[case] input: &str) {
        assert!(SecretChain::parse(input).is_err());
    }

    #[test]
    fn trimming_preserves_candidate_order_and_single_names() {
        let chain = SecretChain::parse(" OP://vault/item/token | github-token \n").unwrap();
        assert!(
            matches!(&chain.candidates()[0], SecretReference::Provider { scheme, payload } if scheme == "op" && payload == "vault/item/token")
        );
        assert!(
            matches!(&chain.candidates()[1], SecretReference::Native(name) if name.as_str() == "github-token")
        );
        assert_eq!(
            SecretChain::parse("github-token")
                .unwrap()
                .candidates()
                .len(),
            1
        );
        assert_eq!(
            SecretChain::parse("same | same")
                .unwrap()
                .candidates()
                .len(),
            2
        );
        assert!(!format!("{chain:?}").contains("vault"));
    }
}
