//! Environment-variable backed secret store.

use secrecy::SecretString;

use crate::{SecretError, SecretStore};

/// Reads secrets from the process environment.
#[derive(Debug, Default, Clone)]
pub struct EnvStore;

impl EnvStore {
    /// Construct (currently nothing to configure).
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl SecretStore for EnvStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(std::env::var(name).ok().map(SecretString::from))
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "env::set_var is unsafe under Rust 2024; uniquely-named keys avoid races"
)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret as _;

    #[test]
    fn reads_env_var() {
        let key = "PAWRLY_TEST_ENV_STORE_VAR_4f1c";
        unsafe { std::env::set_var(key, "secret-value") };
        let s = EnvStore;
        let v = s.get(key).unwrap().unwrap();
        assert_eq!(v.expose_secret(), "secret-value");
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn missing_returns_none() {
        let s = EnvStore;
        assert!(s.get("PAWRLY_DEFINITELY_MISSING_4f1c").unwrap().is_none());
    }
}
