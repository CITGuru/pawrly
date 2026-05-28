//! Secret store backends for Pawrly.
//!
//! Backends:
//!
//! * [`EnvStore`] — reads from process environment.
//! * [`FileStore`] — reads a YAML map from disk; refuses files not at mode 0600.
//! * [`KeyringStore`] — uses the OS keyring (`Keychain` on macOS, etc.).
//! * [`LayeredStore`] — tries one backend after another; first hit wins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use secrecy::{ExposeSecret, SecretString};

mod env;
mod file;
mod keyring;
mod layered;

pub use env::EnvStore;
pub use file::FileStore;
pub use keyring::KeyringStore;
pub use layered::LayeredStore;

pub use secrecy::{ExposeSecret as _, SecretBox, SecretString as Secret};

/// Trait implemented by every secret backend.
///
/// Returns `Ok(Some(_))` if the secret was found, `Ok(None)` if not, and
/// `Err(_)` if the backend itself failed (e.g. file unreadable, keyring
/// locked).
pub trait SecretStore: Send + Sync {
    /// Look up a secret by name.
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError>;
}

/// Error type for secret lookups.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secrets file `{path}` must be mode 0600 (got {mode:o})")]
    InsecureFile { path: PathBuf, mode: u32 },

    #[error("secrets file `{path}` is not valid YAML: {msg}")]
    InvalidFile { path: PathBuf, msg: String },

    #[error("secrets file `{path}`: {msg}")]
    Io { path: PathBuf, msg: String },

    #[error("OS keyring error: {0}")]
    Keyring(String),
}

/// In-memory store, useful for tests.
#[derive(Debug, Default)]
pub struct StaticStore {
    inner: RwLock<HashMap<String, SecretString>>,
}

impl StaticStore {
    /// Construct empty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a secret value.
    pub fn insert(&self, name: impl Into<String>, value: impl Into<String>) {
        let v: String = value.into();
        self.inner
            .write()
            .insert(name.into(), SecretString::from(v));
    }
}

impl SecretStore for StaticStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self
            .inner
            .read()
            .get(name)
            .map(|s| SecretString::from(s.expose_secret().to_string())))
    }
}

/// Default chain of stores: env first, then keyring with `service=pawrly`.
#[must_use]
pub fn default_chain() -> LayeredStore {
    LayeredStore::new(vec![
        Box::new(EnvStore),
        Box::new(KeyringStore::new("pawrly")),
    ])
}

/// Mask a secret for display: show length and last 4 chars.
#[must_use]
pub fn mask(secret: &SecretString) -> String {
    let s = secret.expose_secret();
    let len = s.len();
    if len <= 4 {
        "*".repeat(len)
    } else {
        let tail: String = s
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("****{tail} (len {len})")
    }
}

/// Convenience: read a file's permission mode bits, POSIX only.
#[cfg(unix)]
pub(crate) fn file_mode(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path)?;
    Ok(meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub(crate) fn file_mode(_path: &Path) -> std::io::Result<u32> {
    // On non-Unix, accept files as long as they exist; OS perm model differs.
    Ok(0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_store_round_trips() {
        let s = StaticStore::new();
        s.insert("FOO", "bar");
        let out = s.get("FOO").unwrap().unwrap();
        assert_eq!(out.expose_secret(), "bar");
        assert!(s.get("MISSING").unwrap().is_none());
    }

    #[test]
    fn mask_short_and_long() {
        let s = SecretString::from("ab".to_string());
        assert_eq!(mask(&s), "**");
        let s = SecretString::from("ghp_thisIsLong".to_string());
        let m = mask(&s);
        assert!(m.starts_with("****"));
        assert!(m.contains("Long"));
    }
}
