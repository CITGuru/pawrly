//! YAML-file-backed secret store.
//!
//! File format:
//!
//! ```yaml
//! GITHUB_TOKEN: ghp_xxx
//! LINEAR_API_KEY: lin_xxx
//! ```
//!
//! On Unix, the file must be mode `0600` or it is rejected with
//! [`SecretError::InsecureFile`]. On non-Unix the check is bypassed.

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::RwLock;
use secrecy::{ExposeSecret as _, SecretString};

use crate::{SecretError, SecretStore, file_mode};

/// Secrets loaded from a YAML map on disk.
pub struct FileStore {
    path: PathBuf,
    cache: RwLock<HashMap<String, SecretString>>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .field("entries", &self.cache.read().len())
            .finish()
    }
}

impl FileStore {
    /// Build the store, reading and caching the file immediately.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, SecretError> {
        let path = path.into();
        let cache = RwLock::new(load(&path)?);
        Ok(Self { path, cache })
    }

    /// Re-read the file from disk.
    pub fn reload(&self) -> Result<(), SecretError> {
        let fresh = load(&self.path)?;
        *self.cache.write() = fresh;
        Ok(())
    }

    /// Path to the underlying file.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl SecretStore for FileStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self
            .cache
            .read()
            .get(name)
            .map(|s| SecretString::from(s.expose_secret().to_string())))
    }
}

fn load(path: &std::path::Path) -> Result<HashMap<String, SecretString>, SecretError> {
    let mode = file_mode(path).map_err(|e| SecretError::Io {
        path: path.to_path_buf(),
        msg: e.to_string(),
    })?;
    if mode & 0o077 != 0 {
        return Err(SecretError::InsecureFile {
            path: path.to_path_buf(),
            mode,
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|e| SecretError::Io {
        path: path.to_path_buf(),
        msg: e.to_string(),
    })?;
    let map: HashMap<String, String> =
        serde_yaml::from_str(&raw).map_err(|e| SecretError::InvalidFile {
            path: path.to_path_buf(),
            msg: e.to_string(),
        })?;
    Ok(map
        .into_iter()
        .map(|(k, v)| (k, SecretString::from(v)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    fn write_secrets(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        chmod_600(f.path());
        f
    }

    #[cfg(unix)]
    fn chmod_600(p: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(p).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(p, perms).unwrap();
    }

    #[cfg(not(unix))]
    fn chmod_600(_p: &std::path::Path) {}

    #[test]
    fn loads_yaml() {
        let f = write_secrets("GITHUB_TOKEN: ghp_abc\nLINEAR: lin_xyz\n");
        let store = FileStore::new(f.path()).unwrap();
        assert_eq!(
            store.get("GITHUB_TOKEN").unwrap().unwrap().expose_secret(),
            "ghp_abc"
        );
        assert!(store.get("MISSING").unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_insecure_mode() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"X: y\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(f.path()).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(f.path(), perms).unwrap();

        let err = FileStore::new(f.path()).unwrap_err();
        match err {
            SecretError::InsecureFile { mode, .. } => {
                assert_eq!(mode & 0o777, 0o644);
            }
            other => panic!("expected InsecureFile, got {other:?}"),
        }
    }
}
