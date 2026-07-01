//! Persistence for variable values (static secrets and OAuth refresh tokens), keyed by `VarId`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine as _;
use chacha20poly1305::aead::{Aead as _, KeyInit as _};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use secrecy::{ExposeSecret as _, SecretString};
use sha2::{Digest as _, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum TokenStoreError {
    #[error("variable value store io at `{path}`: {msg}")]
    Io { path: String, msg: String },

    #[error("variable value store at `{path}` is corrupt: {msg}")]
    Corrupt { path: String, msg: String },

    #[error("variable value store keyring: {msg}")]
    Keyring { msg: String },
}

/// Read/write a variable's persisted value by `VarId`.
pub trait VariableValueStore: Send + Sync + std::fmt::Debug {
    fn get(&self, var_id: &str) -> Result<Option<SecretString>, TokenStoreError>;
    fn set(&self, var_id: &str, value: &SecretString) -> Result<(), TokenStoreError>;
    fn delete(&self, var_id: &str) -> Result<(), TokenStoreError>;
}

/// Store key for a literal value, namespaced apart from the bare `VarId` used for
/// refresh tokens so the two never collide.
#[must_use]
pub fn value_key(var_id: &str) -> String {
    format!("value::{var_id}")
}

/// Finds nothing; drops writes.
#[derive(Debug, Default)]
pub struct NoopTokenStore;

impl VariableValueStore for NoopTokenStore {
    fn get(&self, _var_id: &str) -> Result<Option<SecretString>, TokenStoreError> {
        Ok(None)
    }
    fn set(&self, _var_id: &str, _value: &SecretString) -> Result<(), TokenStoreError> {
        Ok(())
    }
    fn delete(&self, _var_id: &str) -> Result<(), TokenStoreError> {
        Ok(())
    }
}

/// Single-file JSON store at mode 0600; a `Mutex` serializes read-modify-write.
#[derive(Debug)]
pub struct FileTokenStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileTokenStore {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn read_all(&self) -> Result<BTreeMap<String, String>, TokenStoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|e| TokenStoreError::Corrupt {
                path: self.path.display().to_string(),
                msg: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(TokenStoreError::Io {
                path: self.path.display().to_string(),
                msg: e.to_string(),
            }),
        }
    }

    fn write_all(&self, map: &BTreeMap<String, String>) -> Result<(), TokenStoreError> {
        let io_err = |msg: String| TokenStoreError::Io {
            path: self.path.display().to_string(),
            msg,
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(e.to_string()))?;
        }
        let body = serde_json::to_string_pretty(map).map_err(|e| io_err(e.to_string()))?;
        std::fs::write(&self.path, body).map_err(|e| io_err(e.to_string()))?;
        set_owner_only(&self.path).map_err(|e| io_err(e.to_string()))?;
        Ok(())
    }
}

impl VariableValueStore for FileTokenStore {
    fn get(&self, var_id: &str) -> Result<Option<SecretString>, TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .read_all()?
            .get(var_id)
            .map(|v| SecretString::from(v.clone())))
    }

    fn set(&self, var_id: &str, value: &SecretString) -> Result<(), TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_all()?;
        map.insert(var_id.to_string(), value.expose_secret().to_string());
        self.write_all(&map)
    }

    fn delete(&self, var_id: &str) -> Result<(), TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_all()?;
        if map.remove(var_id).is_some() {
            self.write_all(&map)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// Variable values in the OS keyring, namespaced under `token::{VarId}`.
#[derive(Debug, Clone)]
pub struct KeyringTokenStore {
    service: String,
}

impl KeyringTokenStore {
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn account(var_id: &str) -> String {
        format!("token::{var_id}")
    }

    fn entry(&self, var_id: &str) -> Result<keyring::Entry, TokenStoreError> {
        keyring::Entry::new(&self.service, &Self::account(var_id))
            .map_err(|e| TokenStoreError::Keyring { msg: e.to_string() })
    }
}

impl VariableValueStore for KeyringTokenStore {
    fn get(&self, var_id: &str) -> Result<Option<SecretString>, TokenStoreError> {
        match self.entry(var_id)?.get_password() {
            Ok(v) => Ok(Some(SecretString::from(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(TokenStoreError::Keyring { msg: e.to_string() }),
        }
    }

    fn set(&self, var_id: &str, value: &SecretString) -> Result<(), TokenStoreError> {
        self.entry(var_id)?
            .set_password(value.expose_secret())
            .map_err(|e| TokenStoreError::Keyring { msg: e.to_string() })
    }

    fn delete(&self, var_id: &str) -> Result<(), TokenStoreError> {
        match self.entry(var_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(TokenStoreError::Keyring { msg: e.to_string() }),
        }
    }
}

/// Variable values in an AEAD-encrypted file (ChaCha20-Poly1305), for hosts
/// without an OS keyring; the 32-byte key lives in a sibling 0600 `key` file.
#[derive(Debug)]
pub struct EncryptedFileTokenStore {
    path: PathBuf,
    key_path: PathBuf,
    lock: Mutex<()>,
}

const TOKEN_KEY_ENV: &str = "PAWRLY_TOKEN_KEY";

impl EncryptedFileTokenStore {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            path: dir.join("tokens.enc"),
            key_path: dir.join("key"),
            lock: Mutex::new(()),
        }
    }

    fn io_err(&self, msg: String) -> TokenStoreError {
        TokenStoreError::Io {
            path: self.path.display().to_string(),
            msg,
        }
    }

    fn corrupt(&self, msg: String) -> TokenStoreError {
        TokenStoreError::Corrupt {
            path: self.path.display().to_string(),
            msg,
        }
    }

    /// The 32-byte cipher key: `$PAWRLY_TOKEN_KEY` (hashed) if set, else the
    /// persisted key file, else a freshly generated key.
    fn resolve_key(&self) -> Result<[u8; 32], TokenStoreError> {
        if let Ok(pass) = std::env::var(TOKEN_KEY_ENV) {
            if !pass.is_empty() {
                let key: [u8; 32] = Sha256::digest(pass.as_bytes()).into();
                self.write_key(&key)?;
                return Ok(key);
            }
        }
        if let Some(existing) = self.read_key()? {
            return Ok(existing);
        }
        let mut key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
        self.write_key(&key)?;
        Ok(key)
    }

    fn read_key(&self) -> Result<Option<[u8; 32]>, TokenStoreError> {
        match std::fs::read_to_string(&self.key_path) {
            Ok(raw) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(raw.trim())
                    .map_err(|e| self.corrupt(format!("key file: {e}")))?;
                let key: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| self.corrupt("key file: expected 32 bytes".to_string()))?;
                Ok(Some(key))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(self.io_err(e.to_string())),
        }
    }

    fn write_key(&self, key: &[u8; 32]) -> Result<(), TokenStoreError> {
        if let Some(parent) = self.key_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| self.io_err(e.to_string()))?;
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        std::fs::write(&self.key_path, encoded).map_err(|e| self.io_err(e.to_string()))?;
        set_owner_only(&self.key_path).map_err(|e| self.io_err(e.to_string()))?;
        Ok(())
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305, TokenStoreError> {
        let key = self.resolve_key()?;
        Ok(ChaCha20Poly1305::new(Key::from_slice(&key)))
    }

    fn read_all(&self) -> Result<BTreeMap<String, String>, TokenStoreError> {
        let blob = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(self.io_err(e.to_string())),
        };
        if blob.len() < 12 {
            return Err(self.corrupt("file shorter than a nonce".to_string()));
        }
        let (nonce, ct) = blob.split_at(12);
        let plain = self
            .cipher()?
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| {
                self.corrupt("decryption failed (wrong key or tampered file)".to_string())
            })?;
        serde_json::from_slice(&plain).map_err(|e| self.corrupt(e.to_string()))
    }

    fn write_all(&self, map: &BTreeMap<String, String>) -> Result<(), TokenStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| self.io_err(e.to_string()))?;
        }
        let plain = serde_json::to_vec(map).map_err(|e| self.io_err(e.to_string()))?;
        let mut nonce = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let ct = self
            .cipher()?
            .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
            .map_err(|e| self.io_err(format!("encryption failed: {e}")))?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        std::fs::write(&self.path, blob).map_err(|e| self.io_err(e.to_string()))?;
        set_owner_only(&self.path).map_err(|e| self.io_err(e.to_string()))?;
        Ok(())
    }
}

impl VariableValueStore for EncryptedFileTokenStore {
    fn get(&self, var_id: &str) -> Result<Option<SecretString>, TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .read_all()?
            .get(var_id)
            .map(|v| SecretString::from(v.clone())))
    }

    fn set(&self, var_id: &str, value: &SecretString) -> Result<(), TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_all()?;
        map.insert(var_id.to_string(), value.expose_secret().to_string());
        self.write_all(&map)
    }

    fn delete(&self, var_id: &str) -> Result<(), TokenStoreError> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut map = self.read_all()?;
        if map.remove(var_id).is_some() {
            self.write_all(&map)?;
        }
        Ok(())
    }
}

/// Forces the encrypted-file backend on headless/daemon/CI hosts where the OS keyring
/// is unreachable or session-scoped.
const NO_KEYRING_ENV: &str = "PAWRLY_NO_KEYRING";

fn keyring_disabled() -> bool {
    matches!(
        std::env::var(NO_KEYRING_ENV).as_deref(),
        Ok("1" | "true" | "yes" | "on")
    )
}

/// Default store: the OS keyring when reachable, else the encrypted-file fallback;
/// only a keyring *error* (not `NoEntry`) falls through.
#[derive(Debug)]
pub struct VariableTokenStore {
    keyring: Option<KeyringTokenStore>,
    file: EncryptedFileTokenStore,
}

impl VariableTokenStore {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            keyring: (!keyring_disabled()).then(|| KeyringTokenStore::new("pawrly")),
            file: EncryptedFileTokenStore::new(dir),
        }
    }

    #[must_use]
    pub fn file_only(dir: PathBuf) -> Self {
        Self {
            keyring: None,
            file: EncryptedFileTokenStore::new(dir),
        }
    }
}

impl VariableValueStore for VariableTokenStore {
    fn get(&self, var_id: &str) -> Result<Option<SecretString>, TokenStoreError> {
        let Some(keyring) = &self.keyring else {
            return self.file.get(var_id);
        };
        match keyring.get(var_id) {
            Ok(found) => Ok(found),
            Err(TokenStoreError::Keyring { .. }) => self.file.get(var_id),
            Err(e) => Err(e),
        }
    }

    fn set(&self, var_id: &str, value: &SecretString) -> Result<(), TokenStoreError> {
        let Some(keyring) = &self.keyring else {
            return self.file.set(var_id, value);
        };
        match keyring.set(var_id, value) {
            Ok(()) => Ok(()),
            Err(TokenStoreError::Keyring { .. }) => self.file.set(var_id, value),
            Err(e) => Err(e),
        }
    }

    fn delete(&self, var_id: &str) -> Result<(), TokenStoreError> {
        let Some(keyring) = &self.keyring else {
            return self.file.delete(var_id);
        };
        match keyring.delete(var_id) {
            Ok(()) => Ok(()),
            Err(TokenStoreError::Keyring { .. }) => self.file.delete(var_id),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn val(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn file_store_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::new(dir.path().join("tokens.json"));
        assert!(store.get("a::T").unwrap().is_none());
        store.set("a::T", &val("rt-1")).unwrap();
        store.set("b::T", &val("rt-2")).unwrap();
        assert_eq!(store.get("a::T").unwrap().unwrap().expose_secret(), "rt-1");
        assert_eq!(store.get("b::T").unwrap().unwrap().expose_secret(), "rt-2");
        store.set("a::T", &val("rt-1b")).unwrap();
        assert_eq!(store.get("a::T").unwrap().unwrap().expose_secret(), "rt-1b");
        let reopened = FileTokenStore::new(dir.path().join("tokens.json"));
        assert_eq!(
            reopened.get("b::T").unwrap().unwrap().expose_secret(),
            "rt-2"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_is_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = FileTokenStore::new(path.clone());
        store.set("x::T", &val("rt")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "value file must be owner-only");
    }

    #[test]
    fn noop_store_finds_nothing() {
        let store = NoopTokenStore;
        store.set("x", &val("rt")).unwrap();
        assert!(store.get("x").unwrap().is_none());
    }

    #[test]
    fn encrypted_store_round_trips_and_hides_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = EncryptedFileTokenStore::new(dir.path().to_path_buf());
        assert!(store.get("a::T").unwrap().is_none());
        store.set("a::T", &val("super-secret-rt")).unwrap();
        assert_eq!(
            store.get("a::T").unwrap().unwrap().expose_secret(),
            "super-secret-rt"
        );

        let raw = std::fs::read(dir.path().join("tokens.enc")).unwrap();
        assert!(
            !raw.windows(b"super-secret-rt".len())
                .any(|w| w == b"super-secret-rt"),
            "value leaked into the file in cleartext"
        );

        let reopened = EncryptedFileTokenStore::new(dir.path().to_path_buf());
        assert_eq!(
            reopened.get("a::T").unwrap().unwrap().expose_secret(),
            "super-secret-rt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn encrypted_store_files_are_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let store = EncryptedFileTokenStore::new(dir.path().to_path_buf());
        store.set("x::T", &val("rt")).unwrap();
        assert_eq!(
            mode(&dir.path().join("tokens.enc")),
            0o600,
            "value file owner-only"
        );
        assert_eq!(mode(&dir.path().join("key")), 0o600, "key file owner-only");
    }

    /// Proves a *real* OS keyring backend is compiled in (not the in-memory mock).
    #[test]
    #[ignore = "touches the real OS keyring; run with --ignored"]
    fn keyring_persists_across_entries() {
        let store = KeyringTokenStore::new("pawrly");
        let id = "pawrly::__keyring_roundtrip_probe";
        let _ = store.delete(id);
        store.set(id, &val("rt-keyring")).unwrap();
        let got = store.get(id).unwrap();
        let _ = store.delete(id);
        assert_eq!(
            got.map(|s| s.expose_secret().to_string()).as_deref(),
            Some("rt-keyring"),
            "keyring did not persist across Entry instances — backend is the mock?"
        );
    }

    /// `file_only` persists to the encrypted file, so one handle's write is read by another.
    #[test]
    fn file_only_round_trips_without_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let store = VariableTokenStore::file_only(dir.path().to_path_buf());
        assert!(store.get("root::LINEAR_TOKEN").unwrap().is_none());
        store.set("root::LINEAR_TOKEN", &val("rt-linear")).unwrap();
        let reopened = VariableTokenStore::file_only(dir.path().to_path_buf());
        assert_eq!(
            reopened
                .get("root::LINEAR_TOKEN")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "rt-linear"
        );
        assert!(dir.path().join("tokens.enc").exists());
    }
}
