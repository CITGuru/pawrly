//! Load `pawrly.yaml` from disk or a string.
//!
//! Pipeline:
//!
//! 1. parse YAML
//! 2. assemble `include:` / `from:` (file path only — see assemble.rs)
//! 3. interpolate `${secret:…}` / `${env:…}` / `${file:…}`
//! 4. schema validate (via serde + per-kind hooks)
//! 5. build `Config`
//!
//! `MaskedConfig` provides a way to render the parsed config without
//! revealing secrets — used by `pawrly source list / show`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use pawrly_core::ConfigError;
use pawrly_secrets::SecretStore;

use crate::assemble;
use crate::interpolate;
use crate::types::Config;
use crate::validator;

pub use crate::assemble::IncludeNode;

/// Load and validate a `pawrly.yaml` from a path.
///
/// Steps: read → parse → assemble (`include:` / `from:`) → interpolate refs
/// (using `secrets`) → validate. Returns the fully resolved `Config` on success.
pub fn load(path: &Path, secrets: &dyn SecretStore) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: serde_json::Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    // Assemble multi-file sources before anything else operates on the tree.
    assemble::assemble(&mut tree, path)?;

    finish(tree, secrets)
}

/// Load and validate a `pawrly.yaml`, building the secret-resolution chain
/// from the file's own `secrets:` block.
///
/// This is the entry point production callers should use: the `secrets:` block
/// (env / file / keyring / auto backends, in order) determines how
/// `${secret:NAME}` references resolve. When the block is omitted, the chain
/// defaults to a single `auto` backend (env, then keyring, then a `.env` file
/// in the config directory if present). Relative `file:` paths and the `auto`
/// `.env` lookup resolve against the config file's directory.
pub fn load_auto(path: &Path) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: serde_json::Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    // Assemble multi-file sources before reading the secrets block.
    assemble::assemble(&mut tree, path)?;

    // Read the secrets backends verbatim (before interpolation — backend defs
    // are literal and must not themselves depend on `${secret:…}`).
    let defs: Vec<crate::types::SecretsBackendDef> = match tree.get("secrets") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| ConfigError::Schema {
            path: "secrets".to_string(),
            msg: e.to_string(),
        })?,
        None => Vec::new(),
    };

    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let store = crate::secrets::build_store(&defs, base_dir)?;

    finish(tree, &store)
}

/// Build the secret-resolution store from a config file's `secrets:` block,
/// without loading the full config. Same chain `load_auto` uses (env / file /
/// keyring / `auto`, in order), so `${secret:NAME}` references and the server's
/// bearer token resolve identically.
pub fn secret_store(path: &Path) -> Result<pawrly_secrets::LayeredStore, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: serde_json::Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;
    assemble::assemble(&mut tree, path)?;
    let defs: Vec<crate::types::SecretsBackendDef> = match tree.get("secrets") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| ConfigError::Schema {
            path: "secrets".to_string(),
            msg: e.to_string(),
        })?,
        None => Vec::new(),
    };
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    crate::secrets::build_store(&defs, base_dir)
}

/// Resolve a single secret by `name` from a config's secret backends, returning
/// the plain value. Used for non-`${secret:}` lookups such as the server's
/// `--bearer-token-from` token.
pub fn resolve_secret(path: &Path, name: &str) -> Result<Option<String>, ConfigError> {
    use pawrly_secrets::SecretStore as _;
    use secrecy::ExposeSecret as _;
    let store = secret_store(path)?;
    let value = store
        .get(name)
        .map_err(|e| ConfigError::Io(e.to_string()))?
        .map(|s| s.expose_secret().to_string());
    Ok(value)
}

/// Same as [`load`] but takes a YAML string. Useful in tests.
///
/// `include:` / `from:` are unavailable here — there is no on-disk parent
/// directory to resolve their paths against, so their presence is an error.
pub fn load_str(raw: &str, secrets: &dyn SecretStore) -> Result<Config, ConfigError> {
    let tree: serde_json::Value =
        serde_yaml::from_str(raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    if assemble::uses_file_primitives(&tree) {
        return Err(ConfigError::Io(
            "include/from: requires a file path; use load(path)".to_string(),
        ));
    }

    finish(tree, secrets)
}

/// Assemble `include:` / `from:` from a file **without** resolving secrets.
///
/// Returns the merged [`Config`] (so `${secret:…}` / `${env:…}` references are
/// preserved verbatim) alongside the originating file of each source, parallel
/// to `config.sources` (`origins[i]` declared `config.sources[i]`).
///
/// Intended for inspection commands like `pawrly config show` and the source
/// origin annotations — not for building an engine, which needs resolved
/// secrets via [`load`].
pub fn assemble_config(path: &Path) -> Result<(Config, Vec<PathBuf>), ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: serde_json::Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    let origins = assemble::assemble(&mut tree, path)?;
    let cfg: Config = serde_json::from_value(tree).map_err(|e| ConfigError::Yaml(e.to_string()))?;
    Ok((cfg, origins))
}

/// Build the `include:` graph rooted at `path` (no merging, no interpolation).
/// Powers `pawrly config show --tree`.
pub fn include_tree(path: &Path) -> Result<IncludeNode, ConfigError> {
    assemble::include_tree(path)
}

/// Shared tail of the load pipeline: interpolate, deserialize, validate.
fn finish(mut tree: serde_json::Value, secrets: &dyn SecretStore) -> Result<Config, ConfigError> {
    // Interpolate references (recursively) on the fully-assembled tree.
    interpolate::resolve(&mut tree, secrets)?;

    // Type-check by deserializing the resolved tree into Config.
    let cfg: Config = serde_json::from_value(tree).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    // Schema-level validation (version + per-source rules).
    let errors = validator::validate(&cfg);
    if !errors.is_empty() {
        return Err(errors
            .0
            .into_iter()
            .next()
            .unwrap_or(ConfigError::Io("validation failed".to_string())));
    }

    Ok(cfg)
}

/// Wrapper that serializes `Config` with secrets replaced by their reference
/// form (e.g. `"${secret:GITHUB_TOKEN}"` → `"****<last4> (len N)"`).
///
/// Currently produces the original `Config` unchanged. Masking
/// of referenced fields (by tracking reference origin during interpolation) is
/// not yet implemented. For now `mask_secrets` is the public hook — callers
/// can rely on the API surface.
#[derive(Debug, Serialize)]
pub struct MaskedConfig<'a>(pub &'a Config);

impl<'a> MaskedConfig<'a> {
    /// Wrap a config for masked display.
    #[must_use]
    pub fn new(cfg: &'a Config) -> Self {
        Self(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_secrets::StaticStore;

    #[test]
    fn loads_minimal_config() {
        let yaml = r#"
version: 1
sources: []
"#;
        let secrets = StaticStore::new();
        let cfg = load_str(yaml, &secrets).unwrap();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.name, "default");
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn loads_with_secret_interpolation() {
        let yaml = r#"
version: 1
sources:
  - name: gh
    kind: github
    config:
      token: ${secret:GITHUB_TOKEN}
"#;
        let secrets = StaticStore::new();
        secrets.insert("GITHUB_TOKEN", "ghp_test_value");
        let cfg = load_str(yaml, &secrets).unwrap();
        assert_eq!(cfg.sources.len(), 1);
        let token = cfg.sources[0].config["token"].as_str().unwrap();
        assert_eq!(token, "ghp_test_value");
    }

    #[test]
    fn rejects_wrong_version() {
        let yaml = r#"
version: 2
sources: []
"#;
        let secrets = StaticStore::new();
        let err = load_str(yaml, &secrets).unwrap_err();
        assert!(matches!(err, ConfigError::UnsupportedVersion(2)));
    }

    #[test]
    fn rejects_unresolved_secret() {
        let yaml = r#"
version: 1
sources:
  - name: gh
    kind: github
    config:
      token: ${secret:MISSING}
"#;
        let secrets = StaticStore::new();
        let err = load_str(yaml, &secrets).unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedSecret(s) if s == "MISSING"));
    }
}
