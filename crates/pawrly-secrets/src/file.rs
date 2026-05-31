//! File-backed secret store, in either YAML or dotenv (`.env`) format.
//!
//! YAML format:
//!
//! ```yaml
//! GITHUB_TOKEN: ghp_xxx
//! LINEAR_API_KEY: lin_xxx
//! ```
//!
//! Dotenv format:
//!
//! ```text
//! # a comment
//! GITHUB_TOKEN=ghp_xxx
//! export LINEAR_API_KEY="lin_xxx"
//! ```
//!
//! The format is chosen by [`FileStore::new`] from the file extension (`.env`,
//! or a filename of `.env`, parses as dotenv; everything else as YAML), or set
//! explicitly with [`FileStore::with_format`].
//!
//! On Unix, the file must be mode `0600` or it is rejected with
//! [`SecretError::InsecureFile`]. On non-Unix the check is bypassed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use secrecy::{ExposeSecret as _, SecretString};

use crate::{SecretError, SecretStore, file_mode};

/// On-disk format of a [`FileStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    /// A YAML map of `KEY: value`.
    Yaml,
    /// Dotenv-style `KEY=value` lines (`#` comments, optional `export`, quotes).
    Dotenv,
}

impl FileFormat {
    /// Pick a format from a path: `.env` extension or a filename of exactly
    /// `.env` parses as dotenv; everything else as YAML.
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        let is_dotenv = path.extension().and_then(|e| e.to_str()) == Some("env")
            || path.file_name().and_then(|n| n.to_str()) == Some(".env");
        if is_dotenv { Self::Dotenv } else { Self::Yaml }
    }
}

/// Secrets loaded from a file on disk (YAML or dotenv).
pub struct FileStore {
    path: PathBuf,
    format: FileFormat,
    cache: RwLock<HashMap<String, SecretString>>,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .field("format", &self.format)
            .field("entries", &self.cache.read().len())
            .finish()
    }
}

impl FileStore {
    /// Build the store, detecting the format from the file extension and
    /// reading/caching the file immediately.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, SecretError> {
        let path = path.into();
        let format = FileFormat::from_path(&path);
        Self::with_format(path, format)
    }

    /// Build the store with an explicit format.
    pub fn with_format(path: impl Into<PathBuf>, format: FileFormat) -> Result<Self, SecretError> {
        let path = path.into();
        let cache = RwLock::new(load(&path, format)?);
        Ok(Self {
            path,
            format,
            cache,
        })
    }

    /// Re-read the file from disk.
    pub fn reload(&self) -> Result<(), SecretError> {
        let fresh = load(&self.path, self.format)?;
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

fn load(
    path: &std::path::Path,
    format: FileFormat,
) -> Result<HashMap<String, SecretString>, SecretError> {
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
    let map = match format {
        FileFormat::Yaml => parse_yaml(path, &raw)?,
        FileFormat::Dotenv => parse_dotenv(&raw),
    };
    Ok(map
        .into_iter()
        .map(|(k, v)| (k, SecretString::from(v)))
        .collect())
}

fn parse_yaml(path: &std::path::Path, raw: &str) -> Result<HashMap<String, String>, SecretError> {
    serde_yaml::from_str(raw).map_err(|e| SecretError::InvalidFile {
        path: path.to_path_buf(),
        msg: e.to_string(),
    })
}

/// Parse dotenv-style `KEY=value` lines. Blank lines and `#` comments are
/// skipped; a leading `export ` is ignored; surrounding single or double
/// quotes around the value are stripped. Lines without `=` are ignored.
fn parse_dotenv(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        map.insert(key.to_string(), strip_quotes(value.trim()).to_string());
    }
    map
}

/// Strip one matching pair of surrounding single or double quotes.
fn strip_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
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
        let store = FileStore::with_format(f.path(), FileFormat::Yaml).unwrap();
        assert_eq!(
            store.get("GITHUB_TOKEN").unwrap().unwrap().expose_secret(),
            "ghp_abc"
        );
        assert!(store.get("MISSING").unwrap().is_none());
    }

    #[test]
    fn loads_dotenv() {
        let f = write_secrets(
            "# creds\nGITHUB_TOKEN=ghp_abc\nexport LINEAR=\"lin_xyz\"\nQUOTED='val ue'\n\nBARE=plain\n",
        );
        let store = FileStore::with_format(f.path(), FileFormat::Dotenv).unwrap();
        assert_eq!(
            store.get("GITHUB_TOKEN").unwrap().unwrap().expose_secret(),
            "ghp_abc"
        );
        assert_eq!(
            store.get("LINEAR").unwrap().unwrap().expose_secret(),
            "lin_xyz"
        );
        assert_eq!(
            store.get("QUOTED").unwrap().unwrap().expose_secret(),
            "val ue"
        );
        assert_eq!(store.get("BARE").unwrap().unwrap().expose_secret(), "plain");
        assert!(store.get("MISSING").unwrap().is_none());
    }

    #[test]
    fn format_detected_from_extension() {
        assert_eq!(
            FileFormat::from_path(Path::new("secrets.yaml")),
            FileFormat::Yaml
        );
        assert_eq!(
            FileFormat::from_path(Path::new("secrets.yml")),
            FileFormat::Yaml
        );
        assert_eq!(
            FileFormat::from_path(Path::new("creds.env")),
            FileFormat::Dotenv
        );
        assert_eq!(FileFormat::from_path(Path::new(".env")), FileFormat::Dotenv);
        assert_eq!(
            FileFormat::from_path(Path::new("/a/b/.env")),
            FileFormat::Dotenv
        );
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

        let err = FileStore::with_format(f.path(), FileFormat::Yaml).unwrap_err();
        match err {
            SecretError::InsecureFile { mode, .. } => {
                assert_eq!(mode & 0o777, 0o644);
            }
            other => panic!("expected InsecureFile, got {other:?}"),
        }
    }
}
