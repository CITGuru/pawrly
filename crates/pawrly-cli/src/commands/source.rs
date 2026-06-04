//! `pawrly source` — manage workspace sources (add | list | remove | refresh | test).
//!
//! Mutating subcommands edit the workspace `pawrly.yaml` AND propagate the
//! change to the running `EngineService` so the active in-process or remote
//! engine reflects reality without a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use clap::{Args as ClapArgs, Subcommand};
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
    /// Register a new source and append it to `pawrly.yaml`.
    Add(AddArgs),
    /// List configured sources.
    List(ListArgs),
    /// Remove a source from the engine and `pawrly.yaml`.
    Remove(RemoveArgs),
    /// Re-enumerate the source's tables in the running engine.
    Refresh(RefreshArgs),
    /// Run a connectivity smoke test against the source.
    Test(TestArgs),
}

#[derive(ClapArgs, Debug)]
pub struct AddArgs {
    /// Source kind: `file`, `http`, `sqlite`, `postgres`, `mysql`, `duckdb`,
    /// `snowflake`, `iceberg`, `ducklake`, `delta`.
    #[arg(value_name = "KIND")]
    pub kind: String,

    /// Logical source name (must be a valid SQL identifier).
    #[arg(long)]
    pub name: String,

    /// Optional human-readable description.
    #[arg(long)]
    pub description: Option<String>,

    /// File path / glob (kinds: `file`, `iceberg`, `delta`, …).
    #[arg(long)]
    pub path: Option<String>,

    /// Base URL (HTTP-shaped sources).
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
    let kind = SourceKind::from_str(&args.kind)
        .map_err(|e| anyhow::anyhow!("invalid source kind `{}`: {e}", args.kind))?;

    let yaml_path = resolve_yaml_path(config.clone())?;
    let mut existing = read_or_init_config(&yaml_path)?;
    if existing.sources.iter().any(|s| s.name == args.name) {
        anyhow::bail!(
            "source `{}` already exists in {}",
            args.name,
            yaml_path.display()
        );
    }

    let source_def = build_source_def(&args, kind)?;

    // Build the engine FIRST so it sees the YAML state without the new source.
    // Calling add_source on the engine is the validation step — only persist to
    // YAML if the engine accepts the source. For the local in-process engine
    // this is a fresh build that disappears at process end; the persisted YAML
    // is what the next invocation will read.
    let engine =
        crate::engine::build_engine(remote, no_remote, home, Some(yaml_path.clone())).await?;
    let engine_def = source_to_engine_def(&source_def);
    let info = engine.add_source(engine_def).await?;

    existing.sources.push(source_def);
    write_config_yaml(&yaml_path, &existing)?;

    println!(
        "added source {} (kind={}, tables={}) → {}",
        info.name,
        info.kind,
        info.table_count,
        yaml_path.display()
    );
    Ok(())
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
    let origins = source_origins(config.clone());

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
        println!(
            "{:<20} {:<10} {:<12} {:>6}  {:<26} origin",
            "name", "kind", "status", "tables", "registered"
        );
        for s in &sources {
            let status = match s.status {
                pawrly_core::SourceStatus::Ok => "ok",
                pawrly_core::SourceStatus::Unavailable => "unavailable",
            };
            let origin = origins.get(&s.name).map_or("-", String::as_str);
            println!(
                "{:<20} {:<10} {:<12} {:>6}  {:<26} {}",
                s.name,
                s.kind,
                status,
                s.table_count,
                s.registered_at.to_rfc3339(),
                origin,
            );
        }
    }
    Ok(())
}

/// Best-effort map of source name → declaring file (relative to the config's
/// directory). Returns an empty map if no local config resolves or it can't be
/// assembled — `source list` still works, origins just show as `-`.
fn source_origins(config: Option<PathBuf>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(path) = resolve_yaml_path(config) else {
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
    let yaml_path = resolve_yaml_path(config.clone())?;
    let svc = crate::engine::build_engine(remote, no_remote, home, Some(yaml_path.clone())).await?;
    let removed = svc.remove_source(&args.name).await?;

    let mut had_yaml_entry = false;
    if yaml_path.exists() {
        let mut cfg = read_or_init_config(&yaml_path)?;
        let before = cfg.sources.len();
        cfg.sources.retain(|s| s.name != args.name);
        had_yaml_entry = cfg.sources.len() != before;
        if had_yaml_entry {
            write_config_yaml(&yaml_path, &cfg)?;
        }
    }

    if !removed && !had_yaml_entry {
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
fn build_source_def(args: &AddArgs, kind: SourceKind) -> anyhow::Result<ConfigSourceDef> {
    let mut config = serde_json::Map::new();

    if let Some(p) = &args.path {
        config.insert("path".into(), Value::String(p.clone()));
    }
    if let Some(u) = &args.url {
        config.insert("url".into(), Value::String(u.clone()));
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
        name: args.name.clone(),
        kind,
        description: args.description.clone(),
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
        config: s.config.clone(),
        cache: s.cache.clone(),
        safety: s.safety.clone(),
        tables: s
            .tables
            .iter()
            .map(|t| pawrly_core::TableDef {
                name: t.name.clone(),
                description: t.description.clone(),
                config: t.body.clone(),
                cache: t.cache.clone(),
                safety: t.safety.clone(),
            })
            .collect(),
        raw_table: s.raw_table,
        raw_table_safety: s.raw_table_safety.clone(),
    }
}

/// Resolve which `pawrly.yaml` to edit. Falls back to `./pawrly.yaml`.
fn resolve_yaml_path(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("PAWRLY_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    Ok(std::env::current_dir()?.join("pawrly.yaml"))
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

    fn add_args(kind: &str, name: &str) -> AddArgs {
        AddArgs {
            kind: kind.into(),
            name: name.into(),
            description: None,
            path: None,
            url: None,
            token: None,
            dsn: None,
            set: Vec::new(),
            raw_table: false,
        }
    }

    #[test]
    fn build_source_def_file_path() {
        let mut a = add_args("file", "data");
        a.path = Some("./fx/*.parquet".into());
        let def = build_source_def(&a, SourceKind::File).unwrap();
        assert_eq!(def.name, "data");
        assert_eq!(def.kind, SourceKind::File);
        assert_eq!(def.config["path"], "./fx/*.parquet");
    }

    #[test]
    fn build_source_def_config_kv_parses_json() {
        let mut a = add_args("http", "api");
        a.token = Some("${secret:API_TOKEN}".into());
        a.set = vec!["per_page=100".into(), "repos=[\"a\",\"b\"]".into()];
        let def = build_source_def(&a, SourceKind::Http).unwrap();
        assert_eq!(def.config["token"], "${secret:API_TOKEN}");
        assert_eq!(def.config["per_page"], 100);
        assert!(def.config["repos"].is_array());
    }

    #[test]
    fn build_source_def_rejects_bad_kv() {
        let mut a = add_args("file", "data");
        a.set = vec!["no_equals_sign".into()];
        let err = build_source_def(&a, SourceKind::File).unwrap_err();
        assert!(err.to_string().contains("KEY=VALUE"));
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
