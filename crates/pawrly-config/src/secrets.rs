//! Build a runtime secret store from the config's `secrets:` block.
//!
//! The `secrets:` list is an ordered chain of backends; `${secret:NAME}` is
//! resolved by trying each in turn (first hit wins, via [`LayeredStore`]).
//! When the block is omitted, the chain defaults to a single
//! [`SecretsBackendDef::Auto`] backend.

use std::path::Path;

use pawrly_core::ConfigError;
use pawrly_secrets::{EnvStore, FileFormat, FileStore, KeyringStore, LayeredStore, SecretStore};

use crate::types::{SecretsBackendDef, SecretsFileFormat};

/// Default service name for the OS keyring.
const KEYRING_SERVICE: &str = "pawrly";

/// Conventional dotenv filename discovered by the `auto` backend.
const DOTENV_FILE: &str = ".env";

impl From<SecretsFileFormat> for Option<FileFormat> {
    fn from(f: SecretsFileFormat) -> Self {
        match f {
            SecretsFileFormat::Auto => None,
            SecretsFileFormat::Yaml => Some(FileFormat::Yaml),
            SecretsFileFormat::Dotenv => Some(FileFormat::Dotenv),
        }
    }
}

/// Build a [`LayeredStore`] from the configured backends.
///
/// `base_dir` is the directory of the config file; relative `file:` paths and
/// the `auto` backend's `.env` lookup resolve against it. An empty `defs`
/// chain is treated as a single `auto` backend.
///
/// Explicitly-configured `file:` backends are loaded eagerly and any error
/// (missing file, bad mode, parse failure) is fatal. The `auto` backend's
/// optional `.env` is best-effort: missing or insecure files are skipped.
pub fn build_store(
    defs: &[SecretsBackendDef],
    base_dir: &Path,
) -> Result<LayeredStore, ConfigError> {
    let mut backends: Vec<Box<dyn SecretStore>> = Vec::new();

    if defs.is_empty() {
        push_auto(&mut backends, base_dir);
        return Ok(LayeredStore::new(backends));
    }

    for def in defs {
        match def {
            SecretsBackendDef::Env => backends.push(Box::new(EnvStore)),
            SecretsBackendDef::Keyring { service } => {
                backends.push(Box::new(KeyringStore::new(service.clone())) as Box<dyn SecretStore>);
            }
            SecretsBackendDef::File { path, format } => {
                let resolved = resolve_path(base_dir, path);
                let store = match Option::<FileFormat>::from(*format) {
                    Some(fmt) => FileStore::with_format(&resolved, fmt),
                    None => FileStore::new(&resolved),
                }
                .map_err(|e| map_file_err(&resolved, e))?;
                backends.push(Box::new(store) as Box<dyn SecretStore>);
            }
            SecretsBackendDef::Auto => push_auto(&mut backends, base_dir),
        }
    }

    Ok(LayeredStore::new(backends))
}

/// Push the `auto` chain: env, keyring, then a best-effort `.env`.
fn push_auto(backends: &mut Vec<Box<dyn SecretStore>>, base_dir: &Path) {
    backends.push(Box::new(EnvStore));
    backends.push(Box::new(KeyringStore::new(KEYRING_SERVICE)) as Box<dyn SecretStore>);

    let dotenv = base_dir.join(DOTENV_FILE);
    if !dotenv.exists() {
        return;
    }
    match FileStore::with_format(&dotenv, FileFormat::Dotenv) {
        Ok(store) => backends.push(Box::new(store) as Box<dyn SecretStore>),
        Err(e) => tracing::warn!(
            path = %dotenv.display(),
            error = %e,
            "ignoring .env for `auto` secrets backend",
        ),
    }
}

/// Resolve a possibly-relative backend path against the config directory.
fn resolve_path(base_dir: &Path, path: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    }
}

fn map_file_err(path: &Path, err: pawrly_secrets::SecretError) -> ConfigError {
    use pawrly_secrets::SecretError;
    match err {
        SecretError::InsecureFile { mode, .. } => ConfigError::InsecureSecretsFile {
            path: path.display().to_string(),
            mode,
        },
        other => ConfigError::ReadFile {
            path: path.display().to_string(),
            msg: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[cfg(unix)]
    fn chmod_600(p: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(p).unwrap().permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(p, perms).unwrap();
    }
    #[cfg(not(unix))]
    fn chmod_600(_p: &Path) {}

    #[test]
    fn empty_defs_yield_auto_chain() {
        let dir = tempfile::tempdir().unwrap();
        let store = build_store(&[], dir.path()).unwrap();
        // env + keyring (no .env present) => 2 backends.
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn auto_picks_up_dotenv() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        let mut f = std::fs::File::create(&env_path).unwrap();
        writeln!(f, "MY_SECRET=from-dotenv").unwrap();
        drop(f);
        chmod_600(&env_path);

        let store = build_store(&[SecretsBackendDef::Auto], dir.path()).unwrap();
        assert_eq!(store.len(), 3);
        // Only meaningful when the file was mode 0600 (skipped on insecure).
        #[cfg(unix)]
        {
            use secrecy::ExposeSecret as _;
            let got = store.get("MY_SECRET").unwrap().unwrap();
            assert_eq!(got.expose_secret(), "from-dotenv");
        }
    }

    #[test]
    fn explicit_missing_file_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let res = build_store(
            &[SecretsBackendDef::File {
                path: "nope.yaml".into(),
                format: SecretsFileFormat::Auto,
            }],
            dir.path(),
        );
        match res {
            Err(ConfigError::ReadFile { .. }) => {}
            other => panic!("expected ReadFile error, got {:?}", other.map(|_| "Ok")),
        }
    }

    #[test]
    fn explicit_relative_dotenv_resolves_against_base() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join("creds.env");
        let mut f = std::fs::File::create(&env_path).unwrap();
        writeln!(f, "K=v").unwrap();
        drop(f);
        chmod_600(&env_path);

        let store = build_store(
            &[SecretsBackendDef::File {
                path: "creds.env".into(),
                format: SecretsFileFormat::Auto,
            }],
            dir.path(),
        )
        .unwrap();
        assert_eq!(store.len(), 1);
    }
}
