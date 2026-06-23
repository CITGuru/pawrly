//! `pawrly source` — manage workspace sources (add | list | remove | refresh | test).
//!
//! Mutating subcommands persist to disk AND propagate the change to the running
//! `EngineService` so the active in-process or remote engine reflects reality
//! without a restart.
//!
//! On-disk layout: each source is its own bare-source file at
//! `<workspace>/sources/<name>.yaml`, pulled into the root `pawrly.yaml` via an
//! `include: [sources/*.yaml]` glob (added with the first source, dropped with
//! the last so the glob never matches zero files). When no `--config` /
//! `$PAWRLY_CONFIG` / `./pawrly.yaml` resolves, the workspace defaults to
//! `$PAWRLY_HOME/pawrly.yaml`, bootstrapped on first write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use clap::{Args as ClapArgs, Subcommand};
use comfy_table::{ContentArrangement, Table};
use serde_json::Value;

use pawrly_config::{Config, ConfigSourceDef};
use pawrly_core::{EngineService, SourceKind};

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: SourceCommand,
}

#[derive(Subcommand, Debug)]
pub enum SourceCommand {
    /// Add a source from a kind + flags, a local file, a URL, or a catalog
    /// name; writes it to `sources/<name>.yaml`.
    Add(AddArgs),
    /// List configured sources.
    List(ListArgs),
    /// Remove a source from the engine and its `sources/<name>.yaml` file.
    Remove(RemoveArgs),
    /// Re-enumerate the source's tables in the running engine.
    Refresh(RefreshArgs),
    /// Run a connectivity smoke test against the source.
    Test(TestArgs),
}

#[derive(ClapArgs, Debug)]
pub struct AddArgs {
    /// What to add. One of:
    /// a source kind to build from flags (`file`, `http`, `mcp`, `sqlite`,
    /// `postgres`, `mysql`, `duckdb`, `snowflake`, `iceberg`, `ducklake`,
    /// `delta`); a local file (`./gh.yaml`); a URL (`https://…/x.yaml`); a
    /// catalog name (`github`) or `<kind>/<name>` (`http/github`).
    #[arg(value_name = "KIND|FILE|URL|NAME")]
    pub target: String,

    /// Logical source name (a valid SQL identifier). Required when adding by
    /// kind; when importing a file/URL/catalog source it renames the import
    /// (default: the spec's own `name:`).
    #[arg(long)]
    pub name: Option<String>,

    /// Optional human-readable description.
    #[arg(long)]
    pub description: Option<String>,

    /// File path / glob (kinds: `file`, `iceberg`, `delta`, …).
    #[arg(long)]
    pub path: Option<String>,

    /// Base URL. For `kind: http` this sets `config.base_url`; for `kind: mcp`,
    /// `config.url`.
    #[arg(long)]
    pub url: Option<String>,

    /// Auth token (HTTP-shaped). Pass `${secret:NAME}` to indirect through the secret store.
    #[arg(long)]
    pub token: Option<String>,

    /// DSN / URL for SQL-engine sources (`postgres`, `mysql`, `snowflake`, …).
    #[arg(long)]
    pub dsn: Option<String>,

    /// Generic per-kind config field. Repeatable. `KEY=VALUE`; VALUE is parsed
    /// as JSON when possible, otherwise treated as a string. Renamed from
    /// `--config` to avoid colliding with the global `--config` flag.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,

    /// HTTP-shaped only: register a raw-HTTP table named after the source.
    #[arg(long)]
    pub raw_table: bool,

    /// Catalog ref (branch, tag, or commit) to fetch a named source from.
    /// Defaults to `main`. Only meaningful for catalog imports.
    #[arg(long = "ref", value_name = "REF")]
    pub catalog_ref: Option<String>,

    /// Skip validating the source against a live engine before writing it.
    /// Lets you import a spec before its `${secret:…}` references are set.
    #[arg(long)]
    pub no_verify: bool,
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct RemoveArgs {
    /// Logical source name to remove.
    pub name: String,
}

#[derive(ClapArgs, Debug)]
pub struct RefreshArgs {
    /// Logical source name to refresh.
    pub name: String,
}

#[derive(ClapArgs, Debug)]
pub struct TestArgs {
    /// Logical source name to probe.
    pub name: String,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    match args.command {
        SourceCommand::Add(a) => run_add(home, config, remote, no_remote, a).await,
        SourceCommand::List(a) => run_list(home, config, remote, no_remote, a).await,
        SourceCommand::Remove(a) => run_remove(home, config, remote, no_remote, a).await,
        SourceCommand::Refresh(a) => run_refresh(home, config, remote, no_remote, a).await,
        SourceCommand::Test(a) => run_test(home, config, remote, no_remote, a).await,
    }
}

async fn run_add(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: AddArgs,
) -> anyhow::Result<()> {
    let target = classify_target(&args.target);
    if !matches!(target, AddTarget::Kind(_)) && has_build_flags(&args) {
        anyhow::bail!(
            "build flags (--path/--url/--token/--dsn/--set/--raw-table) can't be \
             combined with a file, URL, or catalog import"
        );
    }

    let mut source_def = match target {
        AddTarget::Kind(kind) => {
            let name = args
                .name
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--name is required when adding by kind"))?;
            build_source_def(&args, kind, &name)?
        }
        AddTarget::File(path) => load_source_from_file(&path)?,
        AddTarget::Url(url) => fetch_source_from_url(&url).await?,
        AddTarget::Catalog { kind, name } => {
            fetch_source_from_catalog(kind.as_deref(), &name, args.catalog_ref.as_deref()).await?
        }
    };

    // `--name` renames an imported source.
    if let Some(name) = &args.name {
        source_def.name = name.clone();
    }
    if source_def.name.trim().is_empty() {
        anyhow::bail!("source name is empty; pass --name");
    }

    // Reject a name already present as a per-source file or anywhere in the
    // assembled config.
    let yaml_path = resolve_yaml_path(config.clone(), home.as_deref())?;
    let source_path = source_file_path(&yaml_path, &source_def.name);
    if source_path.exists() {
        anyhow::bail!(
            "source `{}` already exists ({})",
            source_def.name,
            source_path.display()
        );
    }
    if yaml_path.exists()
        && let Ok((cfg, _)) = pawrly_config::assemble_config(&yaml_path)
        && cfg.sources.iter().any(|s| s.name == source_def.name)
    {
        anyhow::bail!(
            "source `{}` already exists in {}",
            source_def.name,
            yaml_path.display()
        );
    }

    // add_source is the validation step; a rejected source leaves no file behind.
    let info = if args.no_verify {
        None
    } else {
        let engine =
            crate::engine::build_engine(remote, no_remote, home, Some(yaml_path.clone())).await?;
        Some(engine.add_source(source_to_engine_def(&source_def)).await?)
    };

    // The include glob is wired only now that a file backs it — an empty glob
    // fails to load.
    write_source_file(&source_path, &source_def)?;
    ensure_root_includes_sources_glob(&yaml_path)?;

    match info {
        Some(info) => println!(
            "added source {} (kind={}, tables={}) → {}",
            info.name,
            info.kind,
            info.table_count,
            source_path.display()
        ),
        None => println!(
            "added source {} (kind={}, unverified) → {}",
            source_def.name,
            source_def.kind,
            source_path.display()
        ),
    }
    Ok(())
}

/// Where a source definition comes from, derived from the `add` positional.
#[derive(Debug, PartialEq, Eq)]
enum AddTarget {
    /// A known source kind — build the definition from the flags.
    Kind(SourceKind),
    /// A local YAML file holding a bare single-source spec.
    File(PathBuf),
    /// A remote URL to GET a bare single-source spec from.
    Url(String),
    /// A catalog entry. `kind = Some` for the explicit `<kind>/<name>` form;
    /// `None` triggers the kind-folder probe.
    Catalog { kind: Option<String>, name: String },
}

/// Default source catalog repo (`owner/name`), overridable via `$PAWRLY_REGISTRY`
/// so forks / private mirrors work without a rebuild.
const DEFAULT_REGISTRY: &str = "CITGuru/pawrly";

/// Kind folders probed (in order) when a bare catalog name is given. `http`
/// first because that's where the bulk of the catalog lives.
const CATALOG_KIND_PROBE: [&str; 4] = ["http", "file", "mcp", "openapi"];

/// True if any source-building flag was passed (illegal alongside an import).
fn has_build_flags(args: &AddArgs) -> bool {
    args.path.is_some()
        || args.url.is_some()
        || args.token.is_some()
        || args.dsn.is_some()
        || !args.set.is_empty()
        || args.raw_table
}

/// Classify the `add` positional. Order matters: explicit file/URL spellings and
/// known kinds win before a bare token is treated as a catalog name.
fn classify_target(target: &str) -> AddTarget {
    if target.starts_with("http://") || target.starts_with("https://") {
        return AddTarget::Url(target.to_string());
    }
    // Unambiguous local-file spellings.
    if target.ends_with(".yaml")
        || target.ends_with(".yml")
        || target.starts_with("./")
        || target.starts_with("../")
        || target.starts_with('/')
        || target.starts_with('~')
    {
        return AddTarget::File(PathBuf::from(target));
    }
    // Explicit catalog `<kind>/<name>` (single slash, first segment a real kind).
    if let Some((k, n)) = target.split_once('/')
        && !n.is_empty()
        && !n.contains('/')
        && SourceKind::from_str(k).is_ok()
    {
        return AddTarget::Catalog {
            kind: Some(k.to_string()),
            name: n.to_string(),
        };
    }
    // A bare known kind → build from flags.
    if let Ok(kind) = SourceKind::from_str(target) {
        return AddTarget::Kind(kind);
    }
    // A bare token that happens to be a local file with no yaml suffix.
    if Path::new(target).exists() {
        return AddTarget::File(PathBuf::from(target));
    }
    // Otherwise a bare catalog name.
    AddTarget::Catalog {
        kind: None,
        name: target.to_string(),
    }
}

/// Read and parse a bare single-source spec from a local file.
fn load_source_from_file(path: &Path) -> anyhow::Result<ConfigSourceDef> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    parse_source_spec(&raw, &path.display().to_string())
}

/// GET a bare single-source spec from a URL.
async fn fetch_source_from_url(url: &str) -> anyhow::Result<ConfigSourceDef> {
    let body = http_get_text(url).await?;
    parse_source_spec(&body, url)
}

/// Fetch a source from the catalog. With an explicit kind, one request; with a
/// bare name, probe the kind folders (`http` first) until one resolves.
async fn fetch_source_from_catalog(
    kind: Option<&str>,
    name: &str,
    catalog_ref: Option<&str>,
) -> anyhow::Result<ConfigSourceDef> {
    let repo = std::env::var("PAWRLY_REGISTRY").unwrap_or_else(|_| DEFAULT_REGISTRY.to_string());
    let git_ref = catalog_ref.unwrap_or("main");

    let kinds: Vec<&str> = match kind {
        Some(k) => vec![k],
        None => CATALOG_KIND_PROBE.to_vec(),
    };

    let mut tried = Vec::new();
    for k in kinds {
        let url =
            format!("https://raw.githubusercontent.com/{repo}/{git_ref}/sources/{k}/{name}.yaml");
        match http_get_text_optional(&url).await? {
            Some(body) => return parse_source_spec(&body, &url),
            None => tried.push(url),
        }
    }
    anyhow::bail!(
        "no catalog source named `{name}` in {repo}@{git_ref} (looked under: {})",
        tried.join(", ")
    )
}

/// Parse YAML into a single [`ConfigSourceDef`]. Accepts a bare single source
/// (top-level `name:` + `kind:`) or a fragment with exactly one `sources:` entry.
fn parse_source_spec(raw: &str, origin: &str) -> anyhow::Result<ConfigSourceDef> {
    if let Ok(def) = serde_yaml::from_str::<ConfigSourceDef>(raw) {
        return Ok(def);
    }
    #[derive(serde::Deserialize)]
    struct Fragment {
        #[serde(default)]
        sources: Vec<ConfigSourceDef>,
    }
    match serde_yaml::from_str::<Fragment>(raw) {
        Ok(mut frag) => match frag.sources.len() {
            1 => Ok(frag.sources.remove(0)),
            0 => anyhow::bail!("{origin}: no source found in spec"),
            n => anyhow::bail!("{origin}: expected one source, found {n}"),
        },
        Err(e) => anyhow::bail!("{origin}: not a valid source spec: {e}"),
    }
}

/// GET `url` and return the body, erroring on any non-2xx status.
async fn http_get_text(url: &str) -> anyhow::Result<String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("GET {url} → HTTP {}", status.as_u16());
    }
    resp.text()
        .await
        .map_err(|e| anyhow::anyhow!("read {url}: {e}"))
}

/// GET `url`, returning `None` on a 404 (so a catalog probe can fall through to
/// the next kind folder) and erroring on other non-2xx statuses.
async fn http_get_text_optional(url: &str) -> anyhow::Result<Option<String>> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        anyhow::bail!("GET {url} → HTTP {}", status.as_u16());
    }
    Ok(Some(
        resp.text()
            .await
            .map_err(|e| anyhow::anyhow!("read {url}: {e}"))?,
    ))
}

async fn run_list(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: ListArgs,
) -> anyhow::Result<()> {
    // Best-effort: where each source was declared (root vs. an included file).
    // Resolved from the local config before the engine consumes `config`.
    let origins = source_origins(config.clone(), home.as_deref());

    let svc: Arc<dyn EngineService> =
        crate::engine::build_engine(remote, no_remote, home, config).await?;
    let sources = svc.list_sources().await?;

    if args.json {
        let mut rows = Vec::with_capacity(sources.len());
        for s in &sources {
            let mut v = serde_json::to_value(s)?;
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "origin".to_string(),
                    origins
                        .get(&s.name)
                        .map_or(Value::Null, |o| Value::String(o.clone())),
                );
            }
            rows.push(v);
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        let mut table = Table::new();
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table.set_header(vec![
            "name",
            "kind",
            "status",
            "tables",
            "registered",
            "origin",
        ]);
        for s in &sources {
            let status = match s.status {
                pawrly_core::SourceStatus::Ok => "ok",
                pawrly_core::SourceStatus::Unavailable => "unavailable",
            };
            let origin = origins.get(&s.name).map_or("-", String::as_str);
            table.add_row(vec![
                s.name.clone(),
                s.kind.to_string(),
                status.to_string(),
                s.table_count.to_string(),
                s.registered_at.to_rfc3339(),
                origin.to_string(),
            ]);
        }
        println!("{table}");
    }
    Ok(())
}

/// Best-effort map of source name → declaring file (relative to the config's
/// directory). Returns an empty map if no local config resolves or it can't be
/// assembled — `source list` still works, origins just show as `-`.
fn source_origins(config: Option<PathBuf>, home: Option<&Path>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(path) = resolve_yaml_path(config, home) else {
        return map;
    };
    if !path.exists() {
        return map;
    }
    let root_dir = path.parent().map(Path::to_path_buf);
    if let Ok((cfg, origins)) = pawrly_config::assemble_config(&path) {
        for (s, origin) in cfg.sources.iter().zip(origins.iter()) {
            let label = root_dir
                .as_deref()
                .and_then(|rd| origin.strip_prefix(rd).ok())
                .unwrap_or(origin.as_path())
                .display()
                .to_string();
            map.insert(s.name.clone(), label);
        }
    }
    map
}

async fn run_remove(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: RemoveArgs,
) -> anyhow::Result<()> {
    let yaml_path = resolve_yaml_path(config.clone(), home.as_deref())?;
    let svc = crate::engine::build_engine(remote, no_remote, home, Some(yaml_path.clone())).await?;
    let removed = svc.remove_source(&args.name).await?;

    let mut changed_disk = false;

    // The per-source file (the layout `source add` writes).
    let source_path = source_file_path(&yaml_path, &args.name);
    if source_path.exists() {
        std::fs::remove_file(&source_path)
            .map_err(|e| anyhow::anyhow!("remove {}: {e}", source_path.display()))?;
        changed_disk = true;
    }

    // Any inline entry in the root (back-compat with single-file workspaces),
    // plus dropping the `sources/*.yaml` include once no per-source file remains
    // — an include glob matching nothing would fail the next load.
    if yaml_path.exists() {
        let mut cfg = read_or_init_config(&yaml_path)?;
        let before_sources = cfg.sources.len();
        cfg.sources.retain(|s| s.name != args.name);
        let mut root_changed = cfg.sources.len() != before_sources;
        if !any_source_files(&yaml_path) {
            let before_inc = cfg.include.len();
            cfg.include.retain(|i| i != SOURCES_GLOB);
            root_changed |= cfg.include.len() != before_inc;
        }
        if root_changed {
            write_config_yaml(&yaml_path, &cfg)?;
            changed_disk = true;
        }
    }

    if !removed && !changed_disk {
        anyhow::bail!("source `{}` not found", args.name);
    }
    println!("removed source {}", args.name);
    Ok(())
}

async fn run_refresh(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: RefreshArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let report = svc.refresh_catalog(Some(&args.name)).await?;
    println!(
        "refreshed source {} (sources_refreshed={}, tables_discovered={})",
        args.name, report.sources_refreshed, report.tables_discovered
    );
    Ok(())
}

async fn run_test(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: TestArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let report = svc.test_source(&args.name).await?;
    let status = if report.ok { "ok" } else { "FAIL" };
    let detail = report.detail.as_deref().unwrap_or("");
    println!(
        "{} {} latency={:?} {}",
        report.name, status, report.latency, detail
    );
    if !report.ok {
        std::process::exit(2);
    }
    Ok(())
}

/// Build a `pawrly_config::SourceDef` from the user-facing flags.
fn build_source_def(
    args: &AddArgs,
    kind: SourceKind,
    name: &str,
) -> anyhow::Result<ConfigSourceDef> {
    let mut config = serde_json::Map::new();

    if let Some(p) = &args.path {
        config.insert("path".into(), Value::String(p.clone()));
    }
    if let Some(u) = &args.url {
        // `kind: http` requires `config.base_url`; `kind: mcp` reads `config.url`.
        let key = if kind == SourceKind::Http {
            "base_url"
        } else {
            "url"
        };
        config.insert(key.into(), Value::String(u.clone()));
    }
    if let Some(t) = &args.token {
        config.insert("token".into(), Value::String(t.clone()));
    }
    if let Some(d) = &args.dsn {
        config.insert("dsn".into(), Value::String(d.clone()));
    }
    for entry in &args.set {
        let (k, v) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("`--set {entry}` must be KEY=VALUE"))?;
        if k.is_empty() {
            anyhow::bail!("`--set` key may not be empty (`{entry}`)");
        }
        let parsed = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.into()));
        config.insert(k.to_string(), parsed);
    }

    let config_value = if config.is_empty() {
        Value::Null
    } else {
        Value::Object(config)
    };

    Ok(ConfigSourceDef {
        name: name.to_string(),
        kind,
        description: args.description.clone(),
        wiki: None,
        examples: Vec::new(),
        from: None,
        config: config_value,
        cache: Default::default(),
        safety: None,
        tables: Vec::new(),
        raw_table: args.raw_table,
        raw_table_safety: None,
    })
}

fn source_to_engine_def(s: &ConfigSourceDef) -> pawrly_core::SourceDef {
    pawrly_core::SourceDef {
        name: s.name.clone(),
        kind: s.kind,
        description: s.description.clone(),
        wiki: s.wiki.clone(),
        examples: s.examples.clone(),
        config: s.config.clone(),
        cache: s.cache.clone(),
        safety: s.safety.clone(),
        tables: s
            .tables
            .iter()
            .map(|t| pawrly_core::TableDef {
                name: t.name.clone(),
                description: t.description.clone(),
                wiki: t.wiki.clone(),
                config: t.body.clone(),
                cache: t.cache.clone(),
                safety: t.safety.clone(),
            })
            .collect(),
        raw_table: s.raw_table,
        raw_table_safety: s.raw_table_safety.clone(),
    }
}

/// Resolve which root `pawrly.yaml` to edit.
///
/// Mirrors the engine's read-path discovery (`engine::default_config_path`):
/// `--config` → `$PAWRLY_CONFIG` → `./pawrly.yaml` (if it exists) →
/// `<home>/pawrly.yaml` (the default workspace, where `<home>` is `--home` /
/// `$PAWRLY_HOME` / `~/.pawrly`). The home fallback is returned even if the file
/// doesn't exist yet; the writer bootstraps it on first use.
fn resolve_yaml_path(explicit: Option<PathBuf>, home: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("PAWRLY_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let cwd = std::env::current_dir()?.join("pawrly.yaml");
    if cwd.exists() {
        return Ok(cwd);
    }
    // No local manifest: target the home workspace, creating it on demand.
    if let Some(h) = pawrly_core::resolve_home(home) {
        return Ok(h.join("pawrly.yaml"));
    }
    // No resolvable home ($HOME / $PAWRLY_HOME unset): keep the cwd manifest.
    Ok(cwd)
}

/// The `include:` pattern that pulls every per-source file into the root config.
const SOURCES_GLOB: &str = "sources/*.yaml";

/// Directory holding per-source files, as a sibling of the root `pawrly.yaml`.
fn sources_dir(root: &Path) -> PathBuf {
    root.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("sources"), |p| p.join("sources"))
}

/// Path of the per-source file for `name`: `<root_dir>/sources/<name>.yaml`.
fn source_file_path(root: &Path, name: &str) -> PathBuf {
    sources_dir(root).join(format!("{name}.yaml"))
}

/// True if any `*.yaml` file remains under the workspace's `sources/` dir.
fn any_source_files(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(sources_dir(root)) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|e| {
        e.path()
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml"))
    })
}

/// Serialize a single source as a *bare single-source* file — the SourceDef at
/// the top level, recognised by the assembler via its top-level `kind:`.
fn write_source_file(path: &Path, def: &ConfigSourceDef) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_yaml::to_string(def)
        .map_err(|e| anyhow::anyhow!("serialize {}: {e}", path.display()))?;
    std::fs::write(path, body).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Ensure the root manifest exists and pulls in `sources/*.yaml`. Bootstraps a
/// starter root when missing and adds the include glob only once a per-source
/// file exists — an `include:` glob that matches nothing is a hard load error,
/// so it must never be written for an empty `sources/` dir.
fn ensure_root_includes_sources_glob(root: &Path) -> anyhow::Result<()> {
    let existed = root.exists();
    let mut cfg = read_or_init_config(root)?;
    let has_glob = cfg.include.iter().any(|i| i == SOURCES_GLOB);
    if has_glob && existed {
        return Ok(());
    }
    if !has_glob {
        cfg.include.push(SOURCES_GLOB.to_string());
    }
    write_config_yaml(root, &cfg)
}

/// Read `pawrly.yaml` without secret interpolation, or return a starter
/// `Config` if the file does not yet exist.
///
/// Skipping interpolation here is deliberate — we are *editing* the YAML and
/// must preserve `${secret:…}` references verbatim, not bake their values into
/// the rewritten file.
fn read_or_init_config(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        return Ok(Config {
            version: 1,
            name: "default".into(),
            defaults: Default::default(),
            secrets: Vec::new(),
            include: Vec::new(),
            sources: Vec::new(),
            semantic: None,
            observability: None,
        });
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let cfg: Config =
        serde_yaml::from_str(&raw).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
    Ok(cfg)
}

/// Write `Config` back to `pawrly.yaml` via serde_yaml.
///
/// Comments and key ordering in the original file are not preserved — adopting
/// a comment-aware emitter is tracked separately. Secret references survive as
/// plain strings because [`read_or_init_config`] skips interpolation.
fn write_config_yaml(path: &Path, cfg: &Config) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_yaml::to_string(cfg)
        .map_err(|e| anyhow::anyhow!("serialize {}: {e}", path.display()))?;
    std::fs::write(path, body).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn add_args(target: &str, name: &str) -> AddArgs {
        AddArgs {
            target: target.into(),
            name: Some(name.into()),
            description: None,
            path: None,
            url: None,
            token: None,
            dsn: None,
            set: Vec::new(),
            raw_table: false,
            catalog_ref: None,
            no_verify: false,
        }
    }

    #[test]
    fn build_source_def_file_path() {
        let mut a = add_args("file", "data");
        a.path = Some("./fx/*.parquet".into());
        let def = build_source_def(&a, SourceKind::File, "data").unwrap();
        assert_eq!(def.name, "data");
        assert_eq!(def.kind, SourceKind::File);
        assert_eq!(def.config["path"], "./fx/*.parquet");
    }

    #[test]
    fn build_source_def_config_kv_parses_json() {
        let mut a = add_args("http", "api");
        a.token = Some("${secret:API_TOKEN}".into());
        a.set = vec!["per_page=100".into(), "repos=[\"a\",\"b\"]".into()];
        let def = build_source_def(&a, SourceKind::Http, "api").unwrap();
        assert_eq!(def.config["token"], "${secret:API_TOKEN}");
        assert_eq!(def.config["per_page"], 100);
        assert!(def.config["repos"].is_array());
    }

    #[test]
    fn build_source_def_rejects_bad_kv() {
        let mut a = add_args("file", "data");
        a.set = vec!["no_equals_sign".into()];
        let err = build_source_def(&a, SourceKind::File, "data").unwrap_err();
        assert!(err.to_string().contains("KEY=VALUE"));
    }

    #[test]
    fn build_source_def_http_url_maps_to_base_url() {
        let mut a = add_args("http", "gh");
        a.url = Some("https://api.github.com".into());
        let def = build_source_def(&a, SourceKind::Http, "gh").unwrap();
        assert_eq!(def.config["base_url"], "https://api.github.com");
        assert!(
            def.config.get("url").is_none(),
            "http must not set config.url"
        );
    }

    #[test]
    fn build_source_def_mcp_url_maps_to_url() {
        let mut a = add_args("mcp", "linear");
        a.url = Some("https://mcp.linear.app/mcp".into());
        let def = build_source_def(&a, SourceKind::Mcp, "linear").unwrap();
        assert_eq!(def.config["url"], "https://mcp.linear.app/mcp");
    }

    #[test]
    fn classify_target_detects_each_form() {
        assert_eq!(
            classify_target("https://x/y.yaml"),
            AddTarget::Url("https://x/y.yaml".into())
        );
        assert_eq!(
            classify_target("./gh.yaml"),
            AddTarget::File(PathBuf::from("./gh.yaml"))
        );
        assert_eq!(
            classify_target("spec.yml"),
            AddTarget::File(PathBuf::from("spec.yml"))
        );
        assert_eq!(
            classify_target("http/github"),
            AddTarget::Catalog {
                kind: Some("http".into()),
                name: "github".into()
            }
        );
        assert_eq!(classify_target("http"), AddTarget::Kind(SourceKind::Http));
        assert_eq!(
            classify_target("postgres"),
            AddTarget::Kind(SourceKind::Postgres)
        );
        assert_eq!(
            classify_target("github"),
            AddTarget::Catalog {
                kind: None,
                name: "github".into()
            }
        );
    }

    #[test]
    fn parse_source_spec_accepts_bare_and_fragment() {
        let bare = "name: gh\nkind: http\nconfig:\n  base_url: https://api.github.com\n";
        let def = parse_source_spec(bare, "bare").unwrap();
        assert_eq!(def.name, "gh");
        assert_eq!(def.kind, SourceKind::Http);

        let frag = "sources:\n  - name: gh\n    kind: http\n    config:\n      base_url: https://api.github.com\n";
        let def = parse_source_spec(frag, "frag").unwrap();
        assert_eq!(def.name, "gh");

        let none = "sources: []\n";
        assert!(parse_source_spec(none, "none").is_err());
    }

    #[test]
    fn read_or_init_config_returns_default_when_missing() {
        let tmp = TempDir::new().unwrap();
        let cfg = read_or_init_config(&tmp.path().join("pawrly.yaml")).unwrap();
        assert_eq!(cfg.version, 1);
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn read_then_write_round_trip_preserves_secret_refs() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("pawrly.yaml");
        std::fs::write(
            &p,
            "version: 1\nsources:\n  - name: gh\n    kind: http\n    config:\n      base_url: https://api.example.com\n      token: ${secret:GH}\n",
        )
        .unwrap();
        let cfg = read_or_init_config(&p).unwrap();
        write_config_yaml(&p, &cfg).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(
            raw.contains("${secret:GH}"),
            "secret ref should round-trip; got:\n{raw}"
        );
    }
}
