//! Multi-file config assembly: `include:`, `from:`, and `semantic.include:`.
//!
//! Runs before interpolation so secret resolution and validation operate on
//! the fully-merged tree. Three orthogonal primitives:
//!
//! - `include:` — top-level field, splices other YAML files (glob-aware) into
//!   this config. Each included file is either a *fragment* (carrying
//!   `sources:` / `secrets:` lists) or a *bare single source* (the SourceDef
//!   itself, recognised by a top-level `kind:`, with no `sources:` wrapper).
//!   Either form may also carry a top-level `models:` list — the semantic
//!   models defined over its sources — which is spliced into `semantic.models`,
//!   so one file can fully describe an integration (its source and its models).
//! - `from:` — inside one source, loads the body of that source from another
//!   YAML file, with inline fields overriding the loaded fragment.
//! - `semantic.include:` — splices the *models* of other YAML files (glob-aware)
//!   into `semantic.models`. Each referenced file contains only models (a
//!   top-level `models:` list or a bare sequence), never sources or secrets.
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

/// The product of [`assemble`]: the flattened tree (mutated in place) plus the
/// per-source provenance needed to resolve `${var:NAME}` in lexical scope.
pub(crate) struct Assembled {
    /// Originating file of each source, parallel to the final `tree["sources"]`.
    pub origins: Vec<PathBuf>,
    /// Include chain (root → … → declaring file) of each source, parallel to
    /// `origins`. Used to merge fragment-scoped `variables:` blocks.
    pub source_chains: Vec<Vec<PathBuf>>,
    /// Lifted `variables:` blocks of non-root fragment files, keyed by file.
    /// The root's global block stays on the tree; source-local blocks stay on
    /// their `SourceDef`.
    pub frag_vars: HashMap<PathBuf, Value>,
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
pub(crate) fn assemble(tree: &mut Value, root_path: &Path) -> Result<Assembled, ConfigError> {
    let mut sources: Vec<Value> = Vec::new();
    let mut origins: Vec<PathBuf> = Vec::new();
    let mut source_chains: Vec<Vec<PathBuf>> = Vec::new();
    let mut frag_vars: HashMap<PathBuf, Value> = HashMap::new();
    let mut secrets: Vec<Value> = Vec::new();
    let mut models: Vec<Value> = Vec::new();
    let mut model_origins: Vec<PathBuf> = Vec::new();
    let mut functions: Vec<Value> = Vec::new();
    let mut function_origins: Vec<PathBuf> = Vec::new();
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
        &mut source_chains,
        &mut frag_vars,
        &mut secrets,
        &mut models,
        &mut model_origins,
        &mut functions,
        &mut function_origins,
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

    // Duplicate top-level function `(namespace, name)` pairs across files, with
    // both originating paths (mirroring the duplicate-source check above).
    // Attached functions ride inside source bodies and are checked by the
    // validator, not here.
    let mut seen_fn: HashMap<String, PathBuf> = HashMap::new();
    for (func, origin) in functions.iter().zip(function_origins.iter()) {
        let (Some(ns), Some(name)) = (
            func.get("namespace").and_then(Value::as_str),
            func.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        let key = format!("{ns}.{name}");
        if let Some(prev) = seen_fn.get(&key) {
            return Err(ConfigError::FunctionInvalid {
                namespace: ns.to_string(),
                name: name.to_string(),
                msg: format!(
                    "duplicate function (declared in `{}` and `{}`)",
                    prev.display(),
                    origin.display()
                ),
            });
        }
        seen_fn.insert(key, origin.clone());
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
    if functions.is_empty() {
        obj.remove("functions");
    } else {
        obj.insert("functions".to_string(), Value::Array(functions));
    }

    // Stage 3: splice models into `semantic.models` — both the models carried
    // by `include:`d source fragments and the `semantic.include:` model-only
    // files.
    assemble_semantic_models(obj, root_path, models, model_origins)?;

    Ok(Assembled {
        origins,
        source_chains,
        frag_vars,
    })
}

/// Merge models into the root config's `semantic.models` from three places, in
/// this order: inline `semantic.models` (origin = the root file), then
/// `semantic.include:` model-only files in glob order, then the models carried
/// by `include:`d source fragments (`fragment_models`, in collection order).
/// Duplicate model names — within or across any of those — are rejected with a
/// message naming both originating files. A no-op when there are no include
/// patterns and no fragment models, leaving any `semantic` block untouched.
fn assemble_semantic_models(
    obj: &mut serde_json::Map<String, Value>,
    root_path: &Path,
    fragment_models: Vec<Value>,
    fragment_origins: Vec<PathBuf>,
) -> Result<(), ConfigError> {
    // `semantic.include:` is consumed here whether or not it has entries, but
    // only a real `semantic` mapping carries it.
    let patterns = match obj.get_mut("semantic").and_then(Value::as_object_mut) {
        Some(sem) => {
            let p = parse_include_patterns(sem.get("include"), root_path)?;
            sem.remove("include");
            p
        }
        None => Vec::new(),
    };

    // Nothing to splice: leave the (possibly absent) semantic block as-is.
    if patterns.is_empty() && fragment_models.is_empty() {
        return Ok(());
    }

    let mut models: Vec<Value> = Vec::new();
    let mut origins: Vec<PathBuf> = Vec::new();

    // 1. Inline `semantic.models`.
    if let Some(Value::Array(inline)) = obj.get("semantic").and_then(|s| s.get("models")) {
        for m in inline {
            models.push(m.clone());
            origins.push(root_path.to_path_buf());
        }
    }

    // 2. `semantic.include:` model-only files, in glob order.
    let base_dir = root_path.parent().unwrap_or_else(|| Path::new("."));
    for pattern in patterns {
        for matched in glob_paths(base_dir, &pattern)? {
            for m in load_model_file(&matched)? {
                models.push(m);
                origins.push(matched.clone());
            }
        }
    }

    // 3. Models carried by `include:`d source fragments, in collection order.
    for (m, origin) in fragment_models.into_iter().zip(fragment_origins) {
        models.push(m);
        origins.push(origin);
    }

    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for (m, origin) in models.iter().zip(origins.iter()) {
        if let Some(name) = m.get("name").and_then(Value::as_str) {
            if let Some(prev) = seen.get(name) {
                return Err(ConfigError::SemanticInvalid {
                    model: name.to_string(),
                    msg: format!(
                        "duplicate model name (declared in `{}` and `{}`)",
                        prev.display(),
                        origin.display()
                    ),
                });
            }
            seen.insert(name.to_string(), origin.clone());
        }
    }

    // Ensure a `semantic` mapping exists (fragment models may arrive with no
    // root `semantic:` block), then write the merged model list.
    let sem = obj
        .entry("semantic".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| ConfigError::Yaml("`semantic` must be a mapping".to_string()))?;
    sem.insert("models".to_string(), Value::Array(models));
    Ok(())
}

/// Read a model-only include file into its list of model mappings. The file is
/// either a bare YAML sequence of models or a mapping whose sole key is
/// `models:` — anything else (sources, secrets, version, …) is rejected so an
/// include file cannot introduce non-model config.
fn load_model_file(path: &Path) -> Result<Vec<Value>, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadFile {
        path: path.display().to_string(),
        msg: e.to_string(),
    })?;
    let value: Value = serde_yaml::from_str(&raw)
        .map_err(|e| ConfigError::Yaml(format!("{}: {e}", path.display())))?;

    match value {
        Value::Array(arr) => Ok(arr),
        Value::Object(mut map) => {
            let models = map.remove("models");
            if !map.is_empty() {
                let mut extra: Vec<&str> = map.keys().map(String::as_str).collect();
                extra.sort_unstable();
                return Err(ConfigError::Yaml(format!(
                    "{}: a semantic include file may contain only models; \
                     unexpected key(s): {}",
                    path.display(),
                    extra.join(", ")
                )));
            }
            match models {
                Some(Value::Array(arr)) => Ok(arr),
                Some(Value::Null) | None => Ok(Vec::new()),
                Some(_) => Err(ConfigError::Yaml(format!(
                    "{}: `models` must be a list",
                    path.display()
                ))),
            }
        }
        _ => Err(ConfigError::Yaml(format!(
            "{}: a semantic include file must be a list of models or a mapping with `models:`",
            path.display()
        ))),
    }
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

/// Walk one file's tree, accumulating its sources (with origins), secrets, and
/// any top-level `models:` it carries, then recurse into its `include:` entries.
/// Inline content of a file precedes the content of the files it includes.
#[allow(clippy::too_many_arguments)]
fn collect(
    tree: &mut Value,
    path: &Path,
    is_root: bool,
    sources: &mut Vec<Value>,
    origins: &mut Vec<PathBuf>,
    source_chains: &mut Vec<Vec<PathBuf>>,
    frag_vars: &mut HashMap<PathBuf, Value>,
    secrets: &mut Vec<Value>,
    models: &mut Vec<Value>,
    model_origins: &mut Vec<PathBuf>,
    functions: &mut Vec<Value>,
    function_origins: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<(), ConfigError> {
    let obj = tree.as_object_mut().ok_or_else(|| {
        ConfigError::Yaml(format!(
            "{}: top-level config must be a mapping",
            path.display()
        ))
    })?;

    // A non-root included file may be either a *fragment* (a mapping carrying
    // `sources:` / `secrets:` / `include:` lists) or a *bare single source*
    // (the SourceDef itself, recognised by a top-level `kind:`). The bare form
    // lets `include:` point straight at a one-source file with no `sources:`
    // wrapper. The root config is always a fragment.
    let is_single_source = !is_root && obj.contains_key("kind");

    if !is_root {
        // `name` is root-only in a fragment, but in a bare single source the
        // top-level `name:` is the source's own name, so it's allowed there.
        let root_only: &[&str] = if is_single_source {
            &["version", "defaults"]
        } else {
            &ROOT_ONLY_KEYS
        };
        for key in root_only {
            if obj.contains_key(*key) {
                return Err(ConfigError::Source(
                    path.display().to_string(),
                    format!("key `{key}` is only allowed in the root config"),
                ));
            }
        }
    }

    // A non-root file may carry a top-level `models:` list — the models defined
    // over its sources — which is spliced into `semantic.models` post-merge. The
    // root keeps its models under `semantic.models`, so it's left untouched here.
    if !is_root {
        take_models(obj, path, models, model_origins)?;
    }

    let patterns;
    if is_single_source {
        // Lift `include:` out before the remaining mapping becomes the source
        // body, so a bare-source file may still nest further includes and the
        // `include` key never leaks into the SourceDef. (`models:` was already
        // taken above, so it doesn't leak into the SourceDef either.)
        patterns = parse_include_patterns(obj.get("include"), path)?;
        obj.remove("include");
        let body = std::mem::take(obj);
        sources.push(Value::Object(body));
        origins.push(path.to_path_buf());
        source_chains.push(chain.clone());
    } else {
        // A non-root fragment may carry a top-level `variables:` block — the
        // global/fragment scope visible to the sources it declares. Lift it out
        // (like `secrets:`/`models:`) so it doesn't leak into the merged tree;
        // the root's block stays put as `Config.variables`.
        if !is_root {
            match obj.remove("variables") {
                Some(Value::Null) | None => {}
                Some(vars) => {
                    frag_vars.insert(path.to_path_buf(), vars);
                }
            }
        }

        match obj.remove("sources") {
            Some(Value::Array(arr)) => {
                for s in arr {
                    sources.push(s);
                    origins.push(path.to_path_buf());
                    source_chains.push(chain.clone());
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

        match obj.remove("functions") {
            Some(Value::Array(arr)) => {
                for func in arr {
                    functions.push(func);
                    function_origins.push(path.to_path_buf());
                }
            }
            Some(Value::Null) | None => {}
            Some(_) => {
                return Err(ConfigError::Yaml(format!(
                    "{}: `functions` must be a list",
                    path.display()
                )));
            }
        }

        patterns = parse_include_patterns(obj.get("include"), path)?;
        obj.remove("include");
    }

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
                &mut sub,
                &matched,
                false,
                sources,
                origins,
                source_chains,
                frag_vars,
                secrets,
                models,
                model_origins,
                functions,
                function_origins,
                visited,
                chain,
            )?;

            chain.pop();
        }
    }

    Ok(())
}

/// Remove a non-root file's optional top-level `models:` list and append its
/// entries (with `path` as origin) to the shared model accumulator. A missing
/// or null `models:` is a no-op; a non-list value is an error.
fn take_models(
    obj: &mut serde_json::Map<String, Value>,
    path: &Path,
    models: &mut Vec<Value>,
    model_origins: &mut Vec<PathBuf>,
) -> Result<(), ConfigError> {
    match obj.remove("models") {
        Some(Value::Array(arr)) => {
            for m in arr {
                models.push(m);
                model_origins.push(path.to_path_buf());
            }
            Ok(())
        }
        Some(Value::Null) | None => Ok(()),
        Some(_) => Err(ConfigError::Yaml(format!(
            "{}: `models` must be a list",
            path.display()
        ))),
    }
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
    let has_semantic_include = obj
        .get("semantic")
        .and_then(Value::as_object)
        .map(|s| matches!(s.get("include"), Some(Value::Array(a)) if !a.is_empty()))
        .unwrap_or(false);
    has_include || has_from || has_semantic_include
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
        let tree = json!({"sources": [{"name": "x", "kind": "http", "from": "./x.yaml"}]});
        assert!(uses_file_primitives(&tree));
    }

    #[test]
    fn uses_file_primitives_false_for_plain() {
        let tree = json!({"version": 1, "include": [], "sources": [{"name": "x"}]});
        assert!(!uses_file_primitives(&tree));
    }
}
