//! Multi-file config assembly: `include:` and `from:` expansion.
//!
//! Runs before interpolation so secret resolution and validation operate on
//! the fully-merged tree. Two orthogonal primitives:
//!
//! - `include:` — top-level field, splices the `sources:` (and optional
//!   `secrets:`) lists of other YAML files (glob-aware) into this config.
//! - `from:` — inside one source, loads the body of that source from another
//!   YAML file, with inline fields overriding the loaded fragment.
//!
//! All relative paths resolve against the directory of the *declaring* file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use pawrly_core::ConfigError;

/// Keys that may only appear in the root config, never in an included fragment.
const ROOT_ONLY_KEYS: [&str; 3] = ["version", "name", "defaults"];

/// One node of the `include:` graph, for `pawrly config show --tree`.
#[derive(Debug, Clone)]
pub struct IncludeNode {
    /// The file this node represents.
    pub path: PathBuf,
    /// Files this node pulls in via `include:`, in merge order.
    pub children: Vec<IncludeNode>,
}

/// Expand `include:` (recursively) and `from:` in a parsed config tree.
///
/// `root_path` is the file the tree was read from; relative `include:` / `from:`
/// paths resolve against the declaring file's directory. On return, `tree` has
/// its `include` key removed, `sources` flattened across every included file
/// (with each source's `from` resolved), and `secrets` merged root-first.
///
/// Returns the originating file of each source, parallel to the final
/// `tree["sources"]` array (so `origins[i]` declared `sources[i]`).
pub(crate) fn assemble(tree: &mut Value, root_path: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let mut sources: Vec<Value> = Vec::new();
    let mut origins: Vec<PathBuf> = Vec::new();
    let mut secrets: Vec<Value> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut chain: Vec<PathBuf> = Vec::new();

    visited.insert(canonical(root_path));
    chain.push(root_path.to_path_buf());

    collect(
        tree,
        root_path,
        true,
        &mut sources,
        &mut origins,
        &mut secrets,
        &mut visited,
        &mut chain,
    )?;

    // Stage 2b: resolve each source's `from:` now that we know which file
    // declared it (so the path resolves against that file's directory).
    for (src, origin) in sources.iter_mut().zip(origins.iter()) {
        expand_from(src, origin)?;
    }

    // Duplicate source names are caught post-merge by the validator too, but
    // here we still know the originating file of each source, so the message
    // can point at both files.
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for (src, origin) in sources.iter().zip(origins.iter()) {
        if let Some(name) = src.get("name").and_then(Value::as_str) {
            if let Some(prev) = seen.get(name) {
                return Err(ConfigError::Source(
                    name.to_string(),
                    format!(
                        "duplicate source name (declared in `{}` and `{}`)",
                        prev.display(),
                        origin.display()
                    ),
                ));
            }
            seen.insert(name.to_string(), origin.clone());
        }
    }

    let obj = tree
        .as_object_mut()
        .ok_or_else(|| ConfigError::Yaml("top-level config must be a mapping".to_string()))?;
    obj.insert("sources".to_string(), Value::Array(sources));
    obj.remove("include");
    if secrets.is_empty() {
        obj.remove("secrets");
    } else {
        obj.insert("secrets".to_string(), Value::Array(secrets));
    }
    Ok(origins)
}

/// Build the `include:` graph rooted at `path` without merging or interpolating.
/// Re-reads each file; intended for the `config show --tree` debug view. Cycles
/// surface as [`ConfigError::IncludeCycle`], same as [`assemble`].
pub(crate) fn include_tree(path: &Path) -> Result<IncludeNode, ConfigError> {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut chain: Vec<PathBuf> = Vec::new();
    visited.insert(canonical(path));
    chain.push(path.to_path_buf());
    build_include_node(path, &mut visited, &mut chain)
}

fn build_include_node(
    path: &Path,
    visited: &mut HashSet<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<IncludeNode, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    let tree: Value = serde_yaml::from_str(&raw)
        .map_err(|e| ConfigError::Yaml(format!("{}: {e}", path.display())))?;

    let mut children = Vec::new();
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for pattern in parse_include_patterns(tree.get("include"), path)? {
        for matched in glob_paths(base_dir, &pattern)? {
            let canon = canonical(&matched);
            if visited.contains(&canon) {
                let mut rendered: Vec<String> =
                    chain.iter().map(|p| p.display().to_string()).collect();
                rendered.push(matched.display().to_string());
                return Err(ConfigError::IncludeCycle(rendered.join(" → ")));
            }
            visited.insert(canon);
            chain.push(matched.clone());
            children.push(build_include_node(&matched, visited, chain)?);
            chain.pop();
        }
    }

    Ok(IncludeNode {
        path: path.to_path_buf(),
        children,
    })
}

/// Validate and collect the string patterns from an `include:` value.
fn parse_include_patterns(
    include: Option<&Value>,
    path: &Path,
) -> Result<Vec<String>, ConfigError> {
    match include {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for entry in arr {
                match entry {
                    Value::String(s) => out.push(s.clone()),
                    _ => {
                        return Err(ConfigError::Yaml(format!(
                            "{}: `include` entries must be strings",
                            path.display()
                        )));
                    }
                }
            }
            Ok(out)
        }
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(_) => Err(ConfigError::Yaml(format!(
            "{}: `include` must be a list of paths",
            path.display()
        ))),
    }
}

/// Walk one file's tree, accumulating its sources (with origins) and secrets,
/// then recurse into its `include:` entries. Inline content of a file precedes
/// the content of the files it includes.
#[allow(clippy::too_many_arguments)]
fn collect(
    tree: &mut Value,
    path: &Path,
    is_root: bool,
    sources: &mut Vec<Value>,
    origins: &mut Vec<PathBuf>,
    secrets: &mut Vec<Value>,
    visited: &mut HashSet<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<(), ConfigError> {
    let obj = tree.as_object_mut().ok_or_else(|| {
        ConfigError::Yaml(format!(
            "{}: top-level config must be a mapping",
            path.display()
        ))
    })?;

    if !is_root {
        for key in ROOT_ONLY_KEYS {
            if obj.contains_key(key) {
                return Err(ConfigError::Source(
                    path.display().to_string(),
                    format!("key `{key}` is only allowed in the root config"),
                ));
            }
        }
    }

    match obj.remove("sources") {
        Some(Value::Array(arr)) => {
            for s in arr {
                sources.push(s);
                origins.push(path.to_path_buf());
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => {
            return Err(ConfigError::Yaml(format!(
                "{}: `sources` must be a list",
                path.display()
            )));
        }
    }

    match obj.remove("secrets") {
        Some(Value::Array(arr)) => secrets.extend(arr),
        Some(Value::Null) | None => {}
        Some(_) => {
            return Err(ConfigError::Yaml(format!(
                "{}: `secrets` must be a list",
                path.display()
            )));
        }
    }

    let patterns = parse_include_patterns(obj.get("include"), path)?;
    obj.remove("include");

    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for pattern in patterns {
        for matched in glob_paths(base_dir, &pattern)? {
            let canon = canonical(&matched);
            if visited.contains(&canon) {
                let mut rendered: Vec<String> =
                    chain.iter().map(|p| p.display().to_string()).collect();
                rendered.push(matched.display().to_string());
                return Err(ConfigError::IncludeCycle(rendered.join(" → ")));
            }
            visited.insert(canon);
            chain.push(matched.clone());

            let raw = std::fs::read_to_string(&matched).map_err(|e| ConfigError::ReadFile {
                path: matched.display().to_string(),
                msg: e.to_string(),
            })?;
            let mut sub: Value = serde_yaml::from_str(&raw)
                .map_err(|e| ConfigError::Yaml(format!("{}: {e}", matched.display())))?;

            collect(
                &mut sub, &matched, false, sources, origins, secrets, visited, chain,
            )?;

            chain.pop();
        }
    }

    Ok(())
}

/// Resolve a single source's `from:` reference, deep-merging the inline body
/// over the loaded fragment. No-op for sources without `from:`.
fn expand_from(src: &mut Value, origin: &Path) -> Result<(), ConfigError> {
    let from_str = match src.get("from") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            return Err(ConfigError::Source(
                source_name_of(src),
                "`from:` must be a string path".to_string(),
            ));
        }
        None => return Ok(()),
    };

    let name = source_name_of(src);
    let base_dir = origin.parent().unwrap_or_else(|| Path::new("."));
    let frag_path = resolve_path(base_dir, &from_str);

    let raw = std::fs::read_to_string(&frag_path).map_err(|e| ConfigError::ReadFile {
        path: frag_path.display().to_string(),
        msg: e.to_string(),
    })?;
    let fragment: Value = serde_yaml::from_str(&raw)
        .map_err(|e| ConfigError::Yaml(format!("{}: {e}", frag_path.display())))?;

    let frag_obj = fragment.as_object().ok_or_else(|| {
        ConfigError::Yaml(format!(
            "{}: `from:` target must be a mapping",
            frag_path.display()
        ))
    })?;
    if frag_obj.contains_key("name") || frag_obj.contains_key("kind") {
        return Err(ConfigError::Source(
            name,
            "`from:` target must not set `name` or `kind`".to_string(),
        ));
    }
    if frag_obj.contains_key("from") {
        return Err(ConfigError::Source(
            name,
            "`from:` is not transitive: the target file may not itself contain `from:`".to_string(),
        ));
    }

    // Overlay = the inline source body minus the identity keys; `name`/`kind`
    // are re-applied after the merge so the parent always owns them.
    let mut overlay = src.as_object().cloned().unwrap_or_default();
    overlay.remove("from");
    let name_v = overlay.remove("name");
    let kind_v = overlay.remove("kind");

    let mut merged = fragment;
    deep_merge(&mut merged, &Value::Object(overlay));

    let merged_obj = merged
        .as_object_mut()
        .ok_or_else(|| ConfigError::Yaml("`from:` merge produced a non-mapping".to_string()))?;
    if let Some(n) = name_v {
        merged_obj.insert("name".to_string(), n);
    }
    if let Some(k) = kind_v {
        merged_obj.insert("kind".to_string(), k);
    }

    *src = merged;
    Ok(())
}

/// Recursively merge `overlay` into `base`. Objects merge key-by-key with the
/// overlay winning on conflicts; arrays and scalars replace wholesale.
pub(crate) fn deep_merge(base: &mut Value, overlay: &Value) {
    if let (Value::Object(b), Value::Object(o)) = (&mut *base, overlay) {
        for (k, ov) in o {
            match b.get_mut(k) {
                Some(bv) => deep_merge(bv, ov),
                None => {
                    b.insert(k.clone(), ov.clone());
                }
            }
        }
        return;
    }
    *base = overlay.clone();
}

/// Glob a single include pattern relative to `base_dir`, returning matches in
/// lexicographic order. An empty match set is an error (a missing literal path
/// or a glob that matched nothing).
fn glob_paths(base_dir: &Path, pattern: &str) -> Result<Vec<PathBuf>, ConfigError> {
    let resolved = resolve_path(base_dir, pattern);
    let resolved = resolved.to_string_lossy();

    let entries = glob::glob(&resolved).map_err(|e| ConfigError::ReadFile {
        path: pattern.to_string(),
        msg: e.to_string(),
    })?;

    let mut out = Vec::new();
    for entry in entries {
        let p = entry.map_err(|e| ConfigError::ReadFile {
            path: pattern.to_string(),
            msg: e.to_string(),
        })?;
        out.push(p);
    }

    if out.is_empty() {
        return Err(ConfigError::ReadFile {
            path: pattern.to_string(),
            msg: "no files matched".to_string(),
        });
    }
    out.sort();
    Ok(out)
}

/// Resolve a raw path string against `base_dir`, expanding a leading tilde.
/// Absolute paths (including tilde-expanded ones) are returned as-is.
fn resolve_path(base_dir: &Path, raw: &str) -> PathBuf {
    let expanded = expand_tilde(raw);
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

/// Tilde-expand `~` and `~/…` to `$HOME`. Returns the input unchanged if the
/// expansion can't be performed.
fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    if input == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(input)
}

/// Canonicalize for cycle detection; falls back to the literal path if the
/// file can't be canonicalized (it was already read successfully by callers).
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn source_name_of(src: &Value) -> String {
    src.get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>")
        .to_string()
}

/// True if `tree` uses `include:` or any source `from:` — used to reject those
/// primitives on the in-memory `load_str` path where there's no parent dir.
pub(crate) fn uses_file_primitives(tree: &Value) -> bool {
    let Some(obj) = tree.as_object() else {
        return false;
    };
    let has_include = matches!(obj.get("include"), Some(Value::Array(a)) if !a.is_empty());
    let has_from = obj
        .get("sources")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .any(|s| matches!(s.get("from"), Some(v) if !v.is_null()))
        })
        .unwrap_or(false);
    has_include || has_from
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_merge_object_keys() {
        let mut base = json!({"config": {"a": 1, "b": 2}, "keep": true});
        let overlay = json!({"config": {"b": 9, "c": 3}});
        deep_merge(&mut base, &overlay);
        assert_eq!(
            base,
            json!({"config": {"a": 1, "b": 9, "c": 3}, "keep": true})
        );
    }

    #[test]
    fn deep_merge_arrays_replace() {
        let mut base = json!({"tables": [1, 2, 3]});
        let overlay = json!({"tables": [9]});
        deep_merge(&mut base, &overlay);
        assert_eq!(base, json!({"tables": [9]}));
    }

    #[test]
    fn deep_merge_scalar_over_object() {
        let mut base = json!({"cache": {"mode": "refresh"}});
        let overlay = json!({"cache": "off"});
        deep_merge(&mut base, &overlay);
        assert_eq!(base, json!({"cache": "off"}));
    }

    #[test]
    fn deep_merge_object_over_scalar() {
        let mut base = json!({"cache": "off"});
        let overlay = json!({"cache": {"mode": "ttl"}});
        deep_merge(&mut base, &overlay);
        assert_eq!(base, json!({"cache": {"mode": "ttl"}}));
    }

    #[test]
    fn uses_file_primitives_detects_include() {
        let tree = json!({"version": 1, "include": ["./a.yaml"]});
        assert!(uses_file_primitives(&tree));
    }

    #[test]
    fn uses_file_primitives_detects_from() {
        let tree = json!({"sources": [{"name": "x", "kind": "github", "from": "./x.yaml"}]});
        assert!(uses_file_primitives(&tree));
    }

    #[test]
    fn uses_file_primitives_false_for_plain() {
        let tree = json!({"version": 1, "include": [], "sources": [{"name": "x"}]});
        assert!(!uses_file_primitives(&tree));
    }
}
