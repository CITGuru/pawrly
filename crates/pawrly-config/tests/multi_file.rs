//! Integration tests for multi-file source configs (`include:` / `from:`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::fs;
use std::path::{Path, PathBuf};

use pawrly_config::load;
use pawrly_core::{CachePolicy, ConfigError};
use pawrly_secrets::StaticStore;

/// Write `content` to `dir/rel`, creating parent directories as needed.
fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[test]
fn shipped_multi_file_example_loads() {
    let path = workspace_dir()
        .join("examples")
        .join("multi-file")
        .join("pawrly.yaml");
    let secrets = StaticStore::new();
    for (k, v) in [
        ("SNOWFLAKE_USER", "svc"),
        ("SNOWFLAKE_PASSWORD", "pw"),
        ("GITHUB_TOKEN", "ghp_x"),
        ("LINEAR_API_KEY", "lin_x"),
        ("PG_DSN", "postgres://x"),
        ("STRIPE_API_KEY", "sk_x"),
    ] {
        secrets.insert(k, v);
    }

    let cfg = load(&path, &secrets).unwrap_or_else(|e| panic!("load failed: {e}"));
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    for expected in ["data", "warehouse", "gh", "linear", "oltp", "billing"] {
        assert!(
            names.contains(&expected),
            "missing `{expected}` in {names:?}"
        );
    }

    // `from:` overlay won on the nested key.
    let wh = cfg.sources.iter().find(|s| s.name == "warehouse").unwrap();
    assert_eq!(wh.config["schema"].as_str(), Some("STAGING"));
    assert_eq!(wh.config["database"].as_str(), Some("ANALYTICS"));

    // The model co-located in `github.yaml` was spliced into the semantic layer.
    let sem = cfg
        .semantic
        .expect("co-located model created a semantic block");
    assert!(
        sem.models.iter().any(|m| m.name == "gh_issues"),
        "co-located model missing from semantic.models"
    );
}

#[test]
fn shipped_semantic_multi_file_example_loads() {
    let path = workspace_dir()
        .join("examples")
        .join("semantic-multi-file")
        .join("pawrly.yaml");

    let cfg = load(&path, &StaticStore::new()).unwrap_or_else(|e| panic!("load failed: {e}"));
    let sem = cfg.semantic.expect("semantic block");
    let names: Vec<&str> = sem.models.iter().map(|m| m.name.as_str()).collect();
    for expected in ["orders", "customers", "order_items"] {
        assert!(
            names.contains(&expected),
            "missing `{expected}` in {names:?}"
        );
    }
    // The cross-file relationship resolved into the merged config.
    let orders = sem.models.iter().find(|m| m.name == "orders").unwrap();
    assert!(
        orders
            .relationships
            .iter()
            .any(|r| r.target_model == "order_items"),
        "orders should relate to order_items across files"
    );
}

#[test]
fn include_glob_concatenates_in_lexicographic_order() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    // Created out of order; glob must sort by path before merging.
    write(
        dir.path(),
        "sources/zeta.yaml",
        "sources:\n  - name: zeta\n    kind: http\n",
    );
    write(
        dir.path(),
        "sources/alpha.yaml",
        "sources:\n  - name: alpha\n    kind: http\n",
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
}

#[test]
fn include_accepts_bare_single_source() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    // No `sources:` wrapper — the file *is* the SourceDef, recognised by `kind:`.
    write(
        dir.path(),
        "sources/github.yaml",
        "name: gh\nkind: http\nconfig:\n  token: ${secret:TOKEN}\nraw_table: true\n",
    );

    let secrets = StaticStore::new();
    secrets.insert("TOKEN", "ghp_x");
    let cfg = load(&dir.path().join("pawrly.yaml"), &secrets)
        .unwrap_or_else(|e| panic!("load failed: {e}"));
    let gh = cfg
        .sources
        .iter()
        .find(|s| s.name == "gh")
        .expect("bare single source spliced in");
    assert_eq!(gh.config["token"].as_str(), Some("ghp_x"));
    assert!(gh.raw_table);
}

#[test]
fn include_mixes_bare_source_and_fragment() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    // Bare single-source form.
    write(dir.path(), "sources/github.yaml", "name: gh\nkind: http\n");
    // Fragment (`sources:` list) form — both coexist behind one glob.
    write(
        dir.path(),
        "sources/team.yaml",
        "sources:\n  - name: lin\n    kind: http\n  - name: gh2\n    kind: http\n",
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    for expected in ["gh", "lin", "gh2"] {
        assert!(
            names.contains(&expected),
            "missing `{expected}` in {names:?}"
        );
    }
}

#[test]
fn bare_single_source_rejects_root_only_keys() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./frag.yaml\n",
    );
    // A bare source's top-level `name` is its own, but `defaults` stays root-only.
    write(
        dir.path(),
        "frag.yaml",
        "name: gh\nkind: http\ndefaults:\n  http:\n    timeout: 10s\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Source(_, msg) => {
            assert!(msg.contains("defaults"), "msg: {msg}");
            assert!(
                msg.contains("only allowed in the root config"),
                "msg: {msg}"
            );
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

// ---- include:d fragments carrying their own `models:` (file-level co-location) ----

/// A single model list-entry for a top-level `models:` block, indented for a
/// 2-space `models:` list.
fn model_entry(name: &str, source: &str) -> String {
    format!(
        "  - name: {name}\n    source: {source}\n    \
         dimensions:\n      - {{ name: status, expr: status, type: string }}\n    \
         measures:\n      - {{ name: revenue, agg: sum, expr: total }}\n"
    )
}

#[test]
fn include_fragment_carries_models() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    // A fragment carrying both its source and the model defined over it.
    write(
        dir.path(),
        "sources/github.yaml",
        &format!(
            "sources:\n  - name: gh\n    kind: http\nmodels:\n{}",
            model_entry("gh_issues", "gh.issues")
        ),
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new())
        .unwrap_or_else(|e| panic!("load failed: {e}"));
    let sem = cfg
        .semantic
        .expect("fragment models created a semantic block");
    assert!(
        sem.models.iter().any(|m| m.name == "gh_issues"),
        "model not spliced into semantic.models"
    );
}

#[test]
fn bare_single_source_carries_models() {
    // The headline locality story: one file = a source *and* its models.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    write(
        dir.path(),
        "sources/github.yaml",
        &format!(
            "name: gh\nkind: http\nconfig:\n  token: x\nmodels:\n{}",
            model_entry("gh_issues", "gh.issues")
        ),
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new())
        .unwrap_or_else(|e| panic!("load failed: {e}"));
    // The source registered with its config intact (no `models` leakage)...
    let gh = cfg
        .sources
        .iter()
        .find(|s| s.name == "gh")
        .expect("source gh");
    assert_eq!(gh.config["token"].as_str(), Some("x"));
    // ...and the model landed in the semantic layer.
    let sem = cfg.semantic.expect("semantic block");
    assert!(sem.models.iter().any(|m| m.name == "gh_issues"));
}

#[test]
fn fragment_models_relate_across_source_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    // github.yaml owns the `gh` source and a model that relates to a model
    // defined in a *different* source file — the relationship resolves against
    // the merged model graph.
    write(
        dir.path(),
        "sources/github.yaml",
        "name: gh\nkind: http\nmodels:\n  - name: gh_issues\n    source: gh.issues\n    \
         dimensions:\n      - { name: status, expr: status, type: string }\n    \
         measures:\n      - { name: revenue, agg: sum, expr: total }\n    \
         relationships:\n      - name: ticket\n        kind: many_to_one\n        \
         target: lin_tickets\n        on: this.ticket_id = lin_tickets.id\n",
    );
    write(
        dir.path(),
        "sources/linear.yaml",
        &format!(
            "name: lin\nkind: http\nmodels:\n{}",
            model_entry("lin_tickets", "lin.tickets")
        ),
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new())
        .unwrap_or_else(|e| panic!("cross-source relationship failed: {e}"));
    let sem = cfg.semantic.expect("semantic block");
    let names: Vec<&str> = sem.models.iter().map(|m| m.name.as_str()).collect();
    assert!(
        names.contains(&"gh_issues") && names.contains(&"lin_tickets"),
        "{names:?}"
    );
    let issues = sem.models.iter().find(|m| m.name == "gh_issues").unwrap();
    assert!(
        issues
            .relationships
            .iter()
            .any(|r| r.target_model == "lin_tickets"),
        "cross-file relationship did not resolve"
    );
}

#[test]
fn fragment_model_duplicate_across_files_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    write(
        dir.path(),
        "sources/a.yaml",
        &format!(
            "sources:\n  - name: gh\n    kind: http\nmodels:\n{}",
            model_entry("dup", "gh.x")
        ),
    );
    write(
        dir.path(),
        "sources/b.yaml",
        &format!(
            "sources:\n  - name: lin\n    kind: http\nmodels:\n{}",
            model_entry("dup", "lin.y")
        ),
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::SemanticInvalid { model, msg } => {
            assert_eq!(model, "dup");
            assert!(msg.contains("a.yaml"), "msg: {msg}");
            assert!(msg.contains("b.yaml"), "msg: {msg}");
        }
        other => panic!("expected SemanticInvalid, got {other:?}"),
    }
}

#[test]
fn fragment_model_validated_against_merged_sources() {
    // A fragment model referencing a source that isn't configured anywhere must
    // fail, proving fragment models go through the same post-merge validation.
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sources/*.yaml\n",
    );
    write(
        dir.path(),
        "sources/github.yaml",
        &format!(
            "name: gh\nkind: http\nmodels:\n{}",
            model_entry("orphan", "nope.table")
        ),
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    assert!(
        matches!(&err, ConfigError::SemanticInvalid { msg, .. } if msg.contains("unknown source")),
        "got {err:?}"
    );
}

#[test]
fn from_overlay_matches_worked_example() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "sources/snowflake.yaml",
        r#"
description: Production warehouse
config:
  account: acme.us-east-1
  user: svc
  password: pw
  database: ANALYTICS
  schema: PUBLIC
cache:
  mode: refresh
  every: 1h
tables:
  - name: customer_revenue
    query: SELECT * FROM analytics.public.customer_revenue
"#,
    );
    write(
        dir.path(),
        "pawrly.yaml",
        r#"
version: 1
sources:
  - name: warehouse
    kind: snowflake
    from: ./sources/snowflake.yaml
    config:
      schema: STAGING
    cache:
      mode: ttl
      ttl: 30m
    tables:
      - name: experiments
        query: SELECT * FROM staging.experiments
"#,
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let wh = &cfg.sources[0];
    assert_eq!(wh.name, "warehouse");
    assert_eq!(wh.description.as_deref(), Some("Production warehouse"));
    // config: nested override wins, siblings preserved.
    assert_eq!(wh.config["schema"].as_str(), Some("STAGING"));
    assert_eq!(wh.config["account"].as_str(), Some("acme.us-east-1"));
    assert_eq!(wh.config["database"].as_str(), Some("ANALYTICS"));
    // cache: overlay's mode wins.
    assert!(
        matches!(wh.cache, CachePolicy::Ttl { .. }),
        "cache: {:?}",
        wh.cache
    );
    // tables: arrays replace wholesale.
    assert_eq!(wh.tables.len(), 1);
    assert_eq!(wh.tables[0].name, "experiments");
}

#[test]
fn include_cycle_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.yaml", "version: 1\ninclude:\n  - ./b.yaml\n");
    write(dir.path(), "b.yaml", "include:\n  - ./a.yaml\n");

    let err = load(&dir.path().join("a.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::IncludeCycle(chain) => {
            assert!(chain.contains("a.yaml"), "chain: {chain}");
            assert!(chain.contains("b.yaml"), "chain: {chain}");
        }
        other => panic!("expected IncludeCycle, got {other:?}"),
    }
}

#[test]
fn disallowed_key_in_included_file() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./frag.yaml\n",
    );
    write(
        dir.path(),
        "frag.yaml",
        "defaults:\n  http:\n    timeout: 10s\nsources: []\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Source(_, msg) => {
            assert!(
                msg.contains("only allowed in the root config"),
                "msg: {msg}"
            );
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn from_target_with_name_or_kind_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "body.yaml",
        "name: sneaky\nkind: http\nconfig:\n  token: x\n",
    );
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\nsources:\n  - name: gh\n    kind: http\n    from: ./body.yaml\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Source(_, msg) => {
            assert!(msg.contains("must not set `name` or `kind`"), "msg: {msg}");
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn from_is_not_transitive() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "inner.yaml", "config:\n  a: 1\n");
    write(dir.path(), "body.yaml", "from: ./inner.yaml\n");
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\nsources:\n  - name: gh\n    kind: http\n    from: ./body.yaml\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Source(_, msg) => assert!(msg.contains("not transitive"), "msg: {msg}"),
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn glob_matching_nothing_errors() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./nope/*.yaml\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::ReadFile { path, .. } => assert!(path.contains("nope"), "path: {path}"),
        other => panic!("expected ReadFile error, got {other:?}"),
    }
}

#[test]
fn duplicate_source_names_across_files_name_both_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./one.yaml\n  - ./two.yaml\n",
    );
    write(
        dir.path(),
        "one.yaml",
        "sources:\n  - name: dup\n    kind: http\n",
    );
    write(
        dir.path(),
        "two.yaml",
        "sources:\n  - name: dup\n    kind: http\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Source(name, msg) => {
            assert_eq!(name, "dup");
            assert!(msg.contains("one.yaml"), "msg: {msg}");
            assert!(msg.contains("two.yaml"), "msg: {msg}");
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn from_in_included_file_resolves_against_that_files_dir() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./sub/frag.yaml\n",
    );
    // The `from:` path is relative to sub/, not the root dir.
    write(
        dir.path(),
        "sub/frag.yaml",
        "sources:\n  - name: gh\n    kind: http\n    from: ./body.yaml\n",
    );
    write(
        dir.path(),
        "sub/body.yaml",
        "config:\n  token: from-sub-dir\n",
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let gh = cfg.sources.iter().find(|s| s.name == "gh").unwrap();
    assert_eq!(gh.config["token"].as_str(), Some("from-sub-dir"));
}

#[test]
fn secrets_resolve_in_fragments() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./frag.yaml\n",
    );
    write(
        dir.path(),
        "frag.yaml",
        "sources:\n  - name: gh\n    kind: http\n    config:\n      token: ${secret:TOKEN}\n",
    );

    let secrets = StaticStore::new();
    secrets.insert("TOKEN", "resolved-secret");
    let cfg = load(&dir.path().join("pawrly.yaml"), &secrets).unwrap();
    let gh = cfg.sources.iter().find(|s| s.name == "gh").unwrap();
    assert_eq!(gh.config["token"].as_str(), Some("resolved-secret"));
}

#[test]
fn assemble_config_reports_source_origins() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./frag.yaml\nsources:\n  - name: inline\n    kind: http\n",
    );
    write(
        dir.path(),
        "frag.yaml",
        "sources:\n  - name: included\n    kind: http\n",
    );

    let (cfg, origins) = pawrly_config::assemble_config(&dir.path().join("pawrly.yaml")).unwrap();
    assert_eq!(cfg.sources.len(), origins.len());
    let by_name: std::collections::HashMap<&str, &PathBuf> = cfg
        .sources
        .iter()
        .map(|s| s.name.as_str())
        .zip(origins.iter())
        .collect();
    assert!(by_name[&"inline"].ends_with("pawrly.yaml"));
    assert!(by_name[&"included"].ends_with("frag.yaml"));
}

#[test]
fn assemble_config_preserves_secret_refs() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\nsources:\n  - name: gh\n    kind: http\n    config:\n      token: ${secret:TOKEN}\n",
    );

    let (cfg, _) = pawrly_config::assemble_config(&dir.path().join("pawrly.yaml")).unwrap();
    // No interpolation happens here, so the reference survives verbatim.
    assert_eq!(
        cfg.sources[0].config["token"].as_str(),
        Some("${secret:TOKEN}")
    );
}

#[test]
fn include_tree_reflects_graph() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./a.yaml\n  - ./b.yaml\n",
    );
    write(dir.path(), "a.yaml", "include:\n  - ./nested.yaml\n");
    write(dir.path(), "nested.yaml", "sources: []\n");
    write(dir.path(), "b.yaml", "sources: []\n");

    let root = pawrly_config::include_tree(&dir.path().join("pawrly.yaml")).unwrap();
    assert!(root.path.ends_with("pawrly.yaml"));
    assert_eq!(root.children.len(), 2);
    assert!(root.children[0].path.ends_with("a.yaml"));
    assert_eq!(root.children[0].children.len(), 1);
    assert!(root.children[0].children[0].path.ends_with("nested.yaml"));
    assert!(root.children[1].path.ends_with("b.yaml"));
    assert!(root.children[1].children.is_empty());
}

#[test]
fn load_str_rejects_include() {
    let yaml = "version: 1\ninclude:\n  - ./x.yaml\n";
    let err = pawrly_config::load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Io(msg) if msg.contains("requires a file path")));
}

// ---- semantic.include: model-only multi-file ----

/// A root config with a `data` file source and a `semantic.include` glob.
const SEMANTIC_ROOT: &str = "\
version: 1
sources:
  - name: data
    kind: file
    config:
      path: ./data/*.csv
semantic:
  include:
    - ./models/*.yaml
";

/// One model as a top-level `models:` list entry.
fn model_file(name: &str, table: &str) -> String {
    format!(
        "models:\n  - name: {name}\n    source: data.{table}\n    \
         dimensions:\n      - {{ name: status, expr: status, type: string }}\n    \
         measures:\n      - {{ name: revenue, agg: sum, expr: total }}\n"
    )
}

#[test]
fn semantic_models_split_across_files_merge() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pawrly.yaml", SEMANTIC_ROOT);
    write(
        dir.path(),
        "models/orders.yaml",
        &model_file("orders", "orders"),
    );
    write(
        dir.path(),
        "models/customers.yaml",
        &model_file("customers", "customers"),
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new())
        .unwrap_or_else(|e| panic!("load failed: {e}"));
    let sem = cfg.semantic.expect("semantic block");
    let names: Vec<&str> = sem.models.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"orders"), "{names:?}");
    assert!(names.contains(&"customers"), "{names:?}");
    // The include key is consumed by assembly.
    assert!(sem.include.is_empty());
}

#[test]
fn semantic_include_accepts_bare_sequence() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pawrly.yaml", SEMANTIC_ROOT);
    // A bare YAML sequence of models (no `models:` wrapper).
    write(
        dir.path(),
        "models/orders.yaml",
        "- name: orders\n  source: data.orders\n  \
         dimensions:\n    - { name: status, expr: status, type: string }\n  \
         measures:\n    - { name: revenue, agg: sum, expr: total }\n",
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let sem = cfg.semantic.unwrap();
    assert_eq!(sem.models.len(), 1);
    assert_eq!(sem.models[0].name, "orders");
}

#[test]
fn semantic_include_merges_with_inline_models() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "pawrly.yaml",
        "version: 1
sources:
  - name: data
    kind: file
    config:
      path: ./data/*.csv
semantic:
  include:
    - ./models/*.yaml
  models:
    - name: inline_model
      source: data.inline
      dimensions:
        - { name: status, expr: status, type: string }
      measures:
        - { name: revenue, agg: sum, expr: total }
",
    );
    write(
        dir.path(),
        "models/orders.yaml",
        &model_file("orders", "orders"),
    );

    let cfg = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap();
    let sem = cfg.semantic.unwrap();
    let names: Vec<&str> = sem.models.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"inline_model"), "{names:?}");
    assert!(names.contains(&"orders"), "{names:?}");
}

#[test]
fn duplicate_model_across_files_names_both_files() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pawrly.yaml", SEMANTIC_ROOT);
    write(dir.path(), "models/a.yaml", &model_file("dup", "orders"));
    write(dir.path(), "models/b.yaml", &model_file("dup", "customers"));

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::SemanticInvalid { model, msg } => {
            assert_eq!(model, "dup");
            assert!(msg.contains("a.yaml"), "msg: {msg}");
            assert!(msg.contains("b.yaml"), "msg: {msg}");
        }
        other => panic!("expected SemanticInvalid, got {other:?}"),
    }
}

#[test]
fn semantic_include_file_with_sources_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pawrly.yaml", SEMANTIC_ROOT);
    // An include file that also declares a `sources:` block must be refused.
    write(
        dir.path(),
        "models/sneaky.yaml",
        "sources:\n  - name: evil\n    kind: http\nmodels: []\n",
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    match err {
        ConfigError::Yaml(msg) => {
            assert!(msg.contains("only models"), "msg: {msg}");
            assert!(msg.contains("sources"), "msg: {msg}");
        }
        other => panic!("expected Yaml error, got {other:?}"),
    }
}

#[test]
fn semantic_include_model_validated_against_sources() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pawrly.yaml", SEMANTIC_ROOT);
    // `nope` is not a configured source, so cross-file validation must fail.
    write(
        dir.path(),
        "models/bad.yaml",
        &model_file("orders", "orders").replace("data.orders", "nope.orders"),
    );

    let err = load(&dir.path().join("pawrly.yaml"), &StaticStore::new()).unwrap_err();
    assert!(
        matches!(&err, ConfigError::SemanticInvalid { msg, .. } if msg.contains("unknown source")),
        "got {err:?}"
    );
}

#[test]
fn load_str_rejects_semantic_include() {
    let yaml = "version: 1\nsemantic:\n  include:\n    - ./models/x.yaml\n";
    let err = pawrly_config::load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Io(msg) if msg.contains("requires a file path")));
}
