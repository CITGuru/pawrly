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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use pawrly_core::{ConfigError, DynamicVarBinding};
use pawrly_secrets::{SecretStore, VariableValueStore};

use crate::assemble;
use crate::interpolate;
use crate::types::Config;
use crate::validator;
use crate::variables;

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
    let asm = assemble::assemble(&mut tree, path)?;

    finish(tree, secrets, None, &asm.frag_vars, &asm.source_chains)
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
    load_auto_with_vars(path, None)
}

/// [`load_auto`] that also consults a [`VariableValueStore`] when resolving
/// static `${var:}` secrets — a value persisted by `pawrly source connect` /
/// `pawrly variables set` (keyed by `VarId`) **wins** over the inherited env /
/// secret-chain value. The engine threads its own home-backed store here so
/// connect-stored secrets resolve (and don't hard-fail the load); inspection
/// callers pass `None`.
pub fn load_auto_with_vars(
    path: &Path,
    vars: Option<&dyn VariableValueStore>,
) -> Result<Config, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: serde_json::Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;

    // Assemble multi-file sources before reading the secrets block.
    let asm = assemble::assemble(&mut tree, path)?;

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

    finish(tree, &store, vars, &asm.frag_vars, &asm.source_chains)
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

    // No multi-file assembly here: there are no fragment scopes, and each
    // source's scope is just `global ∪ source-local`.
    finish(tree, secrets, None, &HashMap::new(), &[])
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

    let asm = assemble::assemble(&mut tree, path)?;
    let cfg: Config = serde_json::from_value(tree).map_err(|e| ConfigError::Yaml(e.to_string()))?;
    Ok((cfg, asm.origins))
}

/// Build the `include:` graph rooted at `path` (no merging, no interpolation).
/// Powers `pawrly config show --tree`.
pub fn include_tree(path: &Path) -> Result<IncludeNode, ConfigError> {
    assemble::include_tree(path)
}

/// Shared tail of the load pipeline: interpolate (two passes), deserialize,
/// validate.
///
/// `frag_vars` are the lifted fragment-file `variables:` blocks (keyed by file)
/// and `source_chains` is each source's include chain — both from
/// [`assemble`](crate::assemble); empty for the in-memory `load_str` path.
fn finish(
    mut tree: Value,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
    frag_vars: &HashMap<PathBuf, Value>,
    source_chains: &[Vec<PathBuf>],
) -> Result<Config, ConfigError> {
    // Pass A: `${secret:}` / `${env:}` / `${file:}` over the whole tree
    // (including any `variables:` blocks still on it). `${var:}` is left alone.
    interpolate::resolve(&mut tree, secrets)?;

    // Pass B: per-source `${var:}` resolution. Static refs inline; dynamic refs
    // are left verbatim and returned as bindings (parallel to `tree["sources"]`).
    let bindings = resolve_variables(&mut tree, secrets, vars, frag_vars, source_chains)?;

    // Type-check by deserializing the resolved tree into Config, then attach the
    // dynamic bindings (a `#[serde(skip)]` field, so they survive only here).
    let mut cfg: Config =
        serde_json::from_value(tree).map_err(|e| ConfigError::Yaml(e.to_string()))?;
    for (src, binds) in cfg.sources.iter_mut().zip(bindings) {
        src.dynamic_vars = binds;
    }

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

/// Pass B: per-source `${var:NAME}` resolution. Builds each source's scope as
/// `global ∪ (fragments along its include chain) ∪ source-local`, then inlines
/// static values. Secret/env/file references inside the global block and each
/// source-local block were resolved by Pass A; fragment blocks are resolved here
/// (they were lifted off the tree before Pass A).
fn resolve_variables(
    tree: &mut Value,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
    frag_vars: &HashMap<PathBuf, Value>,
    source_chains: &[Vec<PathBuf>],
) -> Result<Vec<Vec<DynamicVarBinding>>, ConfigError> {
    let global = match tree.get("variables") {
        Some(v) => variables::parse_block(v, "root")?,
        None => variables::VariableScope::new(),
    };

    let mut frag_scopes: HashMap<&PathBuf, variables::VariableScope> = HashMap::new();
    for (file, block) in frag_vars {
        let mut block = block.clone();
        interpolate::resolve(&mut block, secrets)?;
        frag_scopes.insert(
            file,
            variables::parse_block(&block, &file.display().to_string())?,
        );
    }

    let Some(sources) = tree.get_mut("sources").and_then(Value::as_array_mut) else {
        return Ok(Vec::new());
    };

    let mut all = Vec::with_capacity(sources.len());
    for (i, source) in sources.iter_mut().enumerate() {
        // Build the scope from an immutable borrow, releasing it before the
        // mutable `resolve_refs` walk below.
        let scope = source_scope(source, source_chains.get(i), &global, &frag_scopes)?;
        all.push(variables::resolve_refs(source, &scope, secrets, vars)?);
    }

    Ok(all)
}

/// One source's variable scope: `global ∪ (fragments along its include chain) ∪
/// source-local`, inner declarations shadowing outer. Shared by the resolving
/// pass and the read-only [`source_static_vars`] detector.
fn source_scope(
    source: &Value,
    chain: Option<&Vec<PathBuf>>,
    global: &variables::VariableScope,
    frag_scopes: &HashMap<&PathBuf, variables::VariableScope>,
) -> Result<variables::VariableScope, ConfigError> {
    let mut scope = global.clone();
    if let Some(chain) = chain {
        for file in chain {
            if let Some(frag) = frag_scopes.get(file) {
                variables::merge_into(&mut scope, frag);
            }
        }
    }
    if let Some(block) = source.get("variables") {
        let source_name = source.get("name").and_then(Value::as_str).unwrap_or("?");
        let local = variables::parse_block(block, &format!("source:{source_name}"))?;
        variables::merge_into(&mut scope, &local);
    }
    Ok(scope)
}

/// Referenced *static* `${var:NAME}` variables for one source — each with the
/// `VarId` its persisted value is keyed by and whether it resolves now. Powers
/// `pawrly source connect` (and the post-`source add` prompt). Read-only: it
/// resolves nothing into the tree and never errors on a missing value. Passing a
/// `vars` store makes resolution stored-wins-aware (so a connect-stored secret
/// reports `resolves: true`). Returns an empty vec if the source is absent.
/// Dynamic (OAuth) variables are excluded — those are handled by the connect flow.
pub fn source_static_vars(
    path: &Path,
    source_name: &str,
    vars: Option<&dyn VariableValueStore>,
) -> Result<Vec<variables::StaticVarRef>, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
    let mut tree: Value =
        serde_yaml::from_str(&raw).map_err(|e| ConfigError::Yaml(e.to_string()))?;
    let asm = assemble::assemble(&mut tree, path)?;

    // Same secret chain a real load builds, so static secrets resolve identically.
    let defs: Vec<crate::types::SecretsBackendDef> = match tree.get("secrets") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| ConfigError::Schema {
            path: "secrets".to_string(),
            msg: e.to_string(),
        })?,
        None => Vec::new(),
    };
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let store = crate::secrets::build_store(&defs, base_dir)?;

    // Pass A over the whole tree resolves `${secret:}`/`${env:}`/`${file:}` in the
    // global and source-local `variables:` blocks (mirroring `finish`); `${var:}`
    // is left alone. Fragment blocks were lifted off the tree, so resolve them
    // separately, exactly as `resolve_variables` does.
    interpolate::resolve(&mut tree, &store)?;
    let global = match tree.get("variables") {
        Some(v) => variables::parse_block(v, "root")?,
        None => variables::VariableScope::new(),
    };
    let mut frag_scopes: HashMap<&PathBuf, variables::VariableScope> = HashMap::new();
    for (file, block) in &asm.frag_vars {
        let mut block = block.clone();
        interpolate::resolve(&mut block, &store)?;
        frag_scopes.insert(
            file,
            variables::parse_block(&block, &file.display().to_string())?,
        );
    }

    let Some(sources) = tree.get("sources").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    for (i, source) in sources.iter().enumerate() {
        if source.get("name").and_then(Value::as_str) != Some(source_name) {
            continue;
        }
        let scope = source_scope(source, asm.source_chains.get(i), &global, &frag_scopes)?;
        return Ok(variables::collect_static_refs(source, &scope, &store, vars));
    }
    Ok(Vec::new())
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
    kind: http
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
    kind: http
    config:
      token: ${secret:MISSING}
"#;
        let secrets = StaticStore::new();
        let err = load_str(yaml, &secrets).unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedSecret(s) if s == "MISSING"));
    }
}
