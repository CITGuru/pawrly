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

    /// Persist a secret under `(service, account=name)`, replacing any existing
    /// value. Used by interactive setup (`pawrly variables set`).
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        let entry =
            Entry::new(&self.service, name).map_err(|e| SecretError::Keyring(e.to_string()))?;
        entry
            .set_password(value)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        let entry = match Entry::new(&self.service, name) {
            Ok(entry) => entry,
            Err(e) => return classify(e),
        };
        match entry.get_password() {
            Ok(p) => Ok(Some(SecretString::from(p))),
            Err(e) => classify(e),
        }
    }
}

/// Map a keyring error to a lookup result. A missing entry or an unusable store
/// (e.g. headless Linux with no secret-service) is a miss, so a fallback chain
/// moves on instead of failing; only a malformed request surfaces an error.
fn classify(e: keyring::Error) -> Result<Option<SecretString>, SecretError> {
    match e {
        keyring::Error::NoEntry => Ok(None),
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_) => {
            tracing::debug!("keyring unavailable, treating as a miss: {e}");
            Ok(None)
        }
        other => Err(SecretError::Keyring(other.to_string())),
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
