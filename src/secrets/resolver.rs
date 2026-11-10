use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use super::{ProviderError, SecretBytes, SecretChain, SecretProvider, SecretReference};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    InvalidReference,
    Fatal {
        candidate: usize,
        reason: ProviderError,
    },
    Exhausted(Vec<ProviderError>),
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => write!(f, "{}", super::InvalidReference),
            Self::Fatal { candidate, reason } => {
                write!(f, "secret candidate {candidate}: {reason}")
            }
            Self::Exhausted(reasons) => {
                f.write_str("all secret candidates failed")?;
                for (index, reason) in reasons.iter().enumerate() {
                    write!(f, "; candidate {}: {reason}", index + 1)?;
                }
                Ok(())
            }
        }
    }
}
impl std::error::Error for ResolutionError {}

pub struct SecretResolver {
    native: Arc<dyn SecretProvider>,
    providers: HashMap<String, Arc<dyn SecretProvider>>,
}

impl SecretResolver {
    pub fn new(native: Arc<dyn SecretProvider>) -> Self {
        Self {
            native,
            providers: HashMap::new(),
        }
    }

    /// Registration stores a capability; providers must defer session acquisition until resolve.
    pub fn register(
        &mut self,
        scheme: &str,
        provider: Arc<dyn SecretProvider>,
    ) -> Result<(), super::InvalidReference> {
        let reference = SecretReference::parse(&format!("{scheme}://validation"))?;
        let SecretReference::Provider {
            scheme: normalized,
            payload,
        } = reference
        else {
            return Err(super::InvalidReference);
        };
        // A URI passed instead of a scheme must not register under a truncated prefix.
        if !normalized.eq_ignore_ascii_case(scheme) || payload != "validation" {
            return Err(super::InvalidReference);
        }
        self.providers.insert(normalized, provider);
        Ok(())
    }

    pub fn resolve(&self, expression: &str) -> Result<SecretBytes, ResolutionError> {
        let chain =
            SecretChain::parse(expression).map_err(|_| ResolutionError::InvalidReference)?;
        self.resolve_chain(&chain)
    }

    pub fn resolve_chain(&self, chain: &SecretChain) -> Result<SecretBytes, ResolutionError> {
        let mut reasons = Vec::new();
        for (index, reference) in chain.candidates().iter().enumerate() {
            let result = match reference {
                SecretReference::Native(_) => self.native.resolve(reference),
                SecretReference::Provider { scheme, .. } => self
                    .providers
                    .get(scheme)
                    .map_or(Err(ProviderError::Unavailable), |provider| {
                        provider.resolve(reference)
                    }),
            };
            match result {
                Ok(value) => return Ok(value),
                Err(reason) if reason.is_recoverable() => reasons.push(reason),
                Err(reason) => {
                    return Err(ResolutionError::Fatal {
                        candidate: index + 1,
                        reason,
                    });
                }
            }
        }
        Err(ResolutionError::Exhausted(reasons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{CryptoService, NativeProvider, SecretName, SecretStore};
    use rstest::rstest;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Mock {
        calls: Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
        result: Result<&'static [u8], ProviderError>,
    }
    impl SecretProvider for Mock {
        fn resolve(&self, _: &SecretReference) -> Result<SecretBytes, ProviderError> {
            self.calls.lock().unwrap().push(self.label);
            self.result.map(|bytes| SecretBytes::new(bytes.to_vec()))
        }
    }

    #[rstest]
    #[case(ProviderError::Missing, true)]
    #[case(ProviderError::Unavailable, true)]
    #[case(ProviderError::SessionUnavailable, true)]
    #[case(ProviderError::Failed, false)]
    #[case(ProviderError::StoreFailure, false)]
    #[case(ProviderError::DecryptionFailed, false)]
    #[case(ProviderError::InvalidReference, false)]
    fn only_explicitly_recoverable_failures_allow_a_later_provider(
        #[case] reason: ProviderError,
        #[case] succeeds: bool,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut resolver = SecretResolver::new(Arc::new(Mock {
            calls: calls.clone(),
            label: "native",
            result: Err(reason),
        }));
        resolver
            .register(
                "NEXT",
                Arc::new(Mock {
                    calls: calls.clone(),
                    label: "next",
                    result: Ok(b"value"),
                }),
            )
            .unwrap();
        assert_eq!(resolver.resolve("native | next://value").is_ok(), succeeds);
        assert_eq!(calls.lock().unwrap().len(), if succeeds { 2 } else { 1 });
    }

    #[test]
    fn provider_success_does_not_acquire_native_credentials() {
        let directory = tempfile::TempDir::new().unwrap();
        let crypto = Arc::new(CryptoService::for_test(|_| {
            panic!("unused native provider must stay locked")
        }));
        let mut resolver = SecretResolver::new(Arc::new(NativeProvider::new(
            directory.path().to_owned(),
            crypto,
            true,
        )));
        resolver
            .register(
                "first",
                Arc::new(Mock {
                    calls: Arc::default(),
                    label: "first",
                    result: Ok(b"value"),
                }),
            )
            .unwrap();
        assert_eq!(
            resolver.resolve("first://value | native").unwrap().expose(),
            b"value"
        );
        assert!(!directory.path().join(".over").exists());
    }

    #[test]
    fn registration_rejects_uris_in_place_of_schemes() {
        let provider = Arc::new(Mock {
            calls: Arc::default(),
            label: "unused",
            result: Ok(b""),
        });
        let mut resolver = SecretResolver::new(provider.clone());
        assert!(resolver.register("op://payload", provider).is_err());
    }

    #[test]
    fn an_invalid_store_is_fatal_even_when_the_requested_entry_is_absent() {
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::write(directory.path().join(".over"), b"not a directory").unwrap();
        let crypto = Arc::new(CryptoService::for_test(|_| {
            panic!("invalid stores must not unlock")
        }));
        let resolver = SecretResolver::new(Arc::new(NativeProvider::new(
            directory.path().to_owned(),
            crypto,
            true,
        )));
        assert_eq!(
            resolver.resolve("missing | other").unwrap_err(),
            ResolutionError::Fatal {
                candidate: 1,
                reason: ProviderError::StoreFailure
            }
        );
    }

    #[test]
    fn candidates_are_called_in_order_and_stop_at_the_first_success() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let make = |label, result| {
            Arc::new(Mock {
                calls: calls.clone(),
                label,
                result,
            })
        };
        let mut resolver = SecretResolver::new(make("native", Ok(b"")));
        resolver
            .register("one", make("one", Err(ProviderError::SessionUnavailable)))
            .unwrap();
        resolver
            .register("later", make("later", Err(ProviderError::DecryptionFailed)))
            .unwrap();
        assert!(
            resolver
                .resolve("one://value | absent://value | native | later://value")
                .unwrap()
                .expose()
                .is_empty()
        );
        assert_eq!(*calls.lock().unwrap(), ["one", "native"]);
    }

    #[test]
    fn fatal_failures_stop_fallback_and_invalid_chains_do_not_call_providers() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let resolver = SecretResolver::new(Arc::new(Mock {
            calls: calls.clone(),
            label: "native",
            result: Err(ProviderError::DecryptionFailed),
        }));
        assert_eq!(
            resolver.resolve("first | second").unwrap_err(),
            ResolutionError::Fatal {
                candidate: 1,
                reason: ProviderError::DecryptionFailed
            }
        );
        assert_eq!(
            resolver.resolve("first | invalid://").unwrap_err(),
            ResolutionError::InvalidReference
        );
        assert_eq!(*calls.lock().unwrap(), ["native"]);
    }

    #[test]
    fn exhausted_errors_preserve_safe_reasons_without_references() {
        let resolver = SecretResolver::new(Arc::new(Mock {
            calls: Arc::default(),
            label: "native",
            result: Err(ProviderError::Missing),
        }));
        let error = resolver
            .resolve("unknown://sensitive-reference | missing")
            .unwrap_err();
        assert_eq!(
            error,
            ResolutionError::Exhausted(vec![ProviderError::Unavailable, ProviderError::Missing])
        );
        let error = anyhow::Error::new(error);
        for text in [format!("{error:#}"), format!("{error:?}")] {
            assert!(!text.contains("sensitive-reference"));
            assert!(text.contains("candidate"));
        }
    }

    #[test]
    fn native_resolution_is_byte_exact_lazy_and_shares_one_unlock() {
        let directory = tempfile::TempDir::new().unwrap();
        let writer = Arc::new(CryptoService::for_test(|_| Ok(Some("passphrase".into()))));
        let store = SecretStore::new(directory.path(), writer).unwrap();
        for name in ["first", "second"] {
            store
                .add(
                    &SecretName::parse(name).unwrap(),
                    &SecretBytes::new(b"value\0\n".to_vec()),
                    false,
                    false,
                )
                .unwrap();
        }
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let crypto = Arc::new(CryptoService::for_test(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Some("passphrase".into()))
        }));
        let resolver = SecretResolver::new(Arc::new(NativeProvider::new(
            directory.path().to_owned(),
            crypto,
            false,
        )));
        assert!(resolver.resolve("missing").is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        for name in ["absent://value | missing | first", "second"] {
            assert_eq!(resolver.resolve(name).unwrap().expose(), b"value\0\n");
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::write(directory.path().join(".over/secrets/first.age"), b"corrupt").unwrap();
        assert!(matches!(
            resolver.resolve("first | second"),
            Err(ResolutionError::Fatal {
                reason: ProviderError::DecryptionFailed,
                ..
            })
        ));
        let other = tempfile::TempDir::new().unwrap();
        let resolver = SecretResolver::new(Arc::new(NativeProvider::new(
            other.path().to_owned(),
            Arc::new(CryptoService::for_test(|_| {
                panic!("missing secrets must not unlock")
            })),
            false,
        )));
        assert!(matches!(
            resolver.resolve("first"),
            Err(ResolutionError::Exhausted(_))
        ));
        assert!(!other.path().join(".over").exists());
    }
}
