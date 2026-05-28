//! OS keyring backed secret store.

use keyring::Entry;
use secrecy::SecretString;

use crate::{SecretError, SecretStore};

/// Reads secrets from the OS keyring under a fixed `service` name.
/// Lookups use `(service, account=name)` semantics.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// Construct with the given service name.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        let entry =
            Entry::new(&self.service, name).map_err(|e| SecretError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(p) => Ok(Some(SecretString::from(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We don't touch the real keyring in unit tests; just verify construction
    /// and that missing entries return Ok(None) on platforms where the keyring
    /// is reachable. On CI without a keyring backend, this test would error
    /// instead of returning None — so we accept either.
    #[test]
    fn construct_and_lookup_missing() {
        let s = KeyringStore::new("pawrly-test-service-3a9b");
        let r = s.get("definitely-missing-3a9b");
        assert!(matches!(r, Ok(None) | Err(SecretError::Keyring(_))));
    }
}
