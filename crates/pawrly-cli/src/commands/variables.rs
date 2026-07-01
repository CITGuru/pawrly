//! `pawrly variables` — list / set declared source variables.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;

use pawrly_config::{Config, VarKind, VarType, VariableDef};
use pawrly_core::DynamicVarSpec;
use pawrly_secrets::{DevicePrompt, VariableTokenStore, VariableValueStore as _};
use serde_json::Value;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<VariablesCommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum VariablesCommand {
    /// List declared variables (global + source-local scopes).
    List {
        /// Config to read. Defaults to ./pawrly.yaml.
        #[arg(default_value = "./pawrly.yaml")]
        path: PathBuf,
    },
    /// Set a static variable's value (secret or non-secret override).
    Set {
        /// The variable name to set.
        name: String,
        /// Config to read. Defaults to ./pawrly.yaml.
        #[arg(long, default_value = "./pawrly.yaml")]
        config: PathBuf,
    },
}

pub async fn run(home: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    match args.command {
        Some(VariablesCommand::List { path }) => list(&path),
        Some(VariablesCommand::Set { name, config }) => set(home, &config, &name),
        None => list(Path::new("./pawrly.yaml")),
    }
}

/// Resolve a settable static variable (non-secret, static secret, or secret with an `input` method) to its `VarId`; a global declaration wins over a unique source-local one.
fn find_settable_static(cfg: &Config, name: &str) -> anyhow::Result<(String, VariableDef)> {
    let settable = |def: &VariableDef| !def.is_dynamic() || def.has_input_method();

    if let Some(def) = cfg.variables.get(name).filter(|d| settable(d)) {
        return Ok((format!("root::{name}"), def.clone()));
    }

    let mut hits: Vec<(String, VariableDef)> = cfg
        .sources
        .iter()
        .filter_map(|s| {
            s.variables
                .get(name)
                .filter(|d| settable(d))
                .map(|d| (format!("source:{}::{}", s.name, name), d.clone()))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    hits.dedup_by(|a, b| a.0 == b.0);
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => anyhow::bail!(
            "no settable static variable named `{name}` is declared \
             (OAuth secrets are connected via `pawrly source connect`)"
        ),
        _ => anyhow::bail!(
            "`{name}` is declared in multiple sources; set it per source with \
             `pawrly source connect <source> {name}`"
        ),
    }
}

fn set(home: Option<PathBuf>, config: &Path, name: &str) -> anyhow::Result<()> {
    let (cfg, _) = pawrly_config::assemble_config(config)?;
    let (var_id, def) = find_settable_static(&cfg, name)?;

    let value = if def.kind == VarKind::Secret {
        rpassword::prompt_password(format!("Value for `{name}`: "))?
    } else {
        read_visible(&format!("Value for `{name}`{}: ", value_hint(&def)))?
    };
    if value.is_empty() {
        anyhow::bail!("empty value; nothing stored");
    }
    def.coerce(&value)
        .map_err(|e| anyhow::anyhow!("invalid value for `{name}`: {e}"))?;

    token_store(home)?.set(
        &pawrly_secrets::value_key(&var_id),
        &pawrly_secrets::Secret::from(value),
    )?;
    println!("✓ stored `{name}` in the variable value store (id `{var_id}`).");
    Ok(())
}

fn read_visible(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn value_hint(def: &VariableDef) -> String {
    match def.var_type() {
        VarType::Enum => {
            let opts: Vec<String> = def
                .choices
                .iter()
                .map(|c| match c {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            format!(" (one of: {})", opts.join(", "))
        }
        VarType::Boolean => " (true/false)".to_string(),
        VarType::Integer => " (integer)".to_string(),
        VarType::Number => " (number)".to_string(),
        VarType::String => String::new(),
    }
}

pub(crate) fn token_store(home: Option<PathBuf>) -> anyhow::Result<VariableTokenStore> {
    let home = pawrly_core::resolve_home(home.as_deref())
        .ok_or_else(|| anyhow::anyhow!("could not resolve a Pawrly home for value storage"))?;
    Ok(VariableTokenStore::new(home.join("variables")))
}

pub(crate) async fn run_connect(
    home: Option<PathBuf>,
    var_id: &str,
    spec: &DynamicVarSpec,
    name: &str,
) -> anyhow::Result<()> {
    let cache_dir = pawrly_core::resolve_home(home.as_deref()).map(|h| h.join("cache"));
    let tokens = token_store(home)?;
    let client = reqwest::Client::new();

    match spec {
        DynamicVarSpec::ClientCredentials { .. } => {
            println!(
                "`{name}` uses the client_credentials grant — no interactive connect needed; \
                 it is minted automatically at query time."
            );
            Ok(())
        }
        DynamicVarSpec::DeviceCode { .. } => {
            pawrly_secrets::device_code_connect(
                spec,
                &client,
                &tokens,
                var_id,
                cache_dir.as_deref(),
                |p: &DevicePrompt| {
                    let target = p
                        .verification_uri_complete
                        .clone()
                        .unwrap_or_else(|| p.verification_uri.clone());
                    println!("\nTo connect `{name}`:");
                    println!("  1. Open: {target}");
                    println!("  2. Enter the code: {}", p.user_code);
                    println!("\nWaiting for authorization…");
                },
            )
            .await?;
            println!("✓ connected `{name}` (refresh token persisted).");
            Ok(())
        }
        DynamicVarSpec::AuthorizationCode { .. } => {
            pawrly_secrets::authorization_code_connect(
                spec,
                &client,
                &tokens,
                var_id,
                cache_dir.as_deref(),
                |url| {
                    println!("\nTo connect `{name}`, open this URL in your browser:\n  {url}");
                    println!("\nWaiting for the callback…");
                },
            )
            .await?;
            println!("✓ connected `{name}` (refresh token persisted).");
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRow {
    pub scope: String,
    pub name: String,
    pub kind: &'static str,
    pub collection: &'static str,
    pub has_default: bool,
    pub description: String,
}

fn list(path: &Path) -> anyhow::Result<()> {
    let (cfg, _origins) = pawrly_config::assemble_config(path)?;
    let rows = collect_rows(&cfg);
    if rows.is_empty() {
        println!("no variables declared");
        return Ok(());
    }
    println!(
        "{:<14}  {:<22}  {:<8}  {:<11}  {:<8}  DESCRIPTION",
        "SCOPE", "NAME", "KIND", "COLLECTION", "DEFAULT"
    );
    for r in &rows {
        println!(
            "{:<14}  {:<22}  {:<8}  {:<11}  {:<8}  {}",
            r.scope,
            r.name,
            r.kind,
            r.collection,
            if r.has_default { "yes" } else { "-" },
            r.description
        );
    }
    Ok(())
}

#[must_use]
pub fn collect_rows(cfg: &Config) -> Vec<VarRow> {
    let row = |scope: String, name: &str, def: &VariableDef| VarRow {
        scope,
        name: name.to_string(),
        kind: match def.kind {
            VarKind::Variable => "variable",
            VarKind::Secret => "secret",
        },
        collection: if def.is_dynamic() { "oauth" } else { "static" },
        has_default: def.default.is_some(),
        description: def.description.clone().unwrap_or_default(),
    };

    let mut rows: Vec<VarRow> = Vec::new();
    for (name, def) in &cfg.variables {
        rows.push(row("global".to_string(), name, def));
    }
    for s in &cfg.sources {
        for (name, def) in &s.variables {
            rows.push(row(format!("source:{}", s.name), name, def));
        }
    }
    rows.sort_by(|a, b| (&a.scope, &a.name).cmp(&(&b.scope, &b.name)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_secrets::StaticStore;

    fn cfg(yaml: &str) -> Config {
        pawrly_config::load_str(yaml, &StaticStore::new()).expect("parse")
    }

    #[test]
    fn lists_global_and_source_local() {
        let c = cfg(r#"
version: 1
variables:
  API_BASE:
    kind: variable
    default: https://api.example.com
    description: Base URL
sources:
  - name: gh
    kind: http
    variables:
      GH_TOKEN:
        kind: secret
        oauth:
          grant:
            type: device_code
          endpoints:
            device_authorization_url: https://gh/device/code
            token_url: https://gh/token
          client:
            id: { default: cid }
    config:
      base_url: ${var:API_BASE}
      token: ${var:GH_TOKEN}
"#);
        let rows = collect_rows(&c);
        assert_eq!(rows.len(), 2);

        let api = rows.iter().find(|r| r.name == "API_BASE").unwrap();
        assert_eq!(api.scope, "global");
        assert_eq!(api.kind, "variable");
        assert_eq!(api.collection, "static");
        assert!(api.has_default);
        assert_eq!(api.description, "Base URL");

        let tok = rows.iter().find(|r| r.name == "GH_TOKEN").unwrap();
        assert_eq!(tok.scope, "source:gh");
        assert_eq!(tok.kind, "secret");
        assert_eq!(tok.collection, "oauth");
        assert!(!tok.has_default);
    }

    #[test]
    fn empty_when_no_variables() {
        let c = cfg("version: 1\nsources: []\n");
        assert!(collect_rows(&c).is_empty());
    }

    const WITH_OAUTH: &str = r#"
version: 1
variables:
  API_BASE: { kind: variable, default: https://api.example.com }
sources:
  - name: gh
    kind: http
    variables:
      GH_TOKEN:
        kind: secret
        oauth:
          grant: { type: device_code }
          endpoints:
            device_authorization_url: https://gh/device/code
            token_url: https://gh/token
          client: { id: { default: cid } }
    config:
      base_url: ${var:API_BASE}
      token: ${var:GH_TOKEN}
"#;

    const WITH_SECRETS: &str = r#"
version: 1
variables:
  API_TOKEN: { kind: secret }
  CUSTOM:
    kind: secret
    input: MY_KEY
sources:
  - name: gh
    kind: http
    variables:
      LOCAL_TOKEN: { kind: secret }
    config:
      base_url: https://gh
"#;

    #[test]
    fn find_settable_static_resolves_var_id() {
        let c = cfg(WITH_SECRETS);
        assert_eq!(
            find_settable_static(&c, "API_TOKEN").unwrap().0,
            "root::API_TOKEN"
        );
        assert_eq!(
            find_settable_static(&c, "CUSTOM").unwrap().0,
            "root::CUSTOM"
        );
        assert_eq!(
            find_settable_static(&c, "LOCAL_TOKEN").unwrap().0,
            "source:gh::LOCAL_TOKEN"
        );
    }

    #[test]
    fn find_settable_static_rejects_oauth_allows_nonsecret() {
        let c = cfg(WITH_OAUTH);
        assert!(
            find_settable_static(&c, "GH_TOKEN").is_err(),
            "oauth secret uses source connect"
        );
        assert_eq!(
            find_settable_static(&c, "API_BASE").unwrap().0,
            "root::API_BASE"
        );
        assert!(find_settable_static(&c, "NOPE").is_err());
    }

    #[test]
    fn find_settable_static_allows_multimethod_secret() {
        let yaml = r#"
version: 1
sources:
  - name: gh
    kind: http
    variables:
      GH_TOKEN:
        kind: secret
        methods:
          - type: oauth
            grant: { type: device_code }
            endpoints: { device_authorization_url: https://gh/d, token_url: https://gh/t }
            client: { id: { default: cid } }
          - type: input
            input: GH_PAT
      OAUTH_ONLY:
        kind: secret
        oauth:
          grant: { type: device_code }
          endpoints: { device_authorization_url: https://gh/d, token_url: https://gh/t }
          client: { id: { default: cid } }
    config:
      base_url: https://gh
"#;
        let c = cfg(yaml);
        assert_eq!(
            find_settable_static(&c, "GH_TOKEN").unwrap().0,
            "source:gh::GH_TOKEN",
            "a multi-method secret with an input method is settable"
        );
        assert!(
            find_settable_static(&c, "OAUTH_ONLY").is_err(),
            "an OAuth-only secret still goes through source connect"
        );
    }
}
