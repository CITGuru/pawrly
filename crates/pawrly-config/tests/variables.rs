//! Integration tests for declared source variables (`variables:` / `${var:NAME}`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::fs;
use std::path::{Path, PathBuf};

use pawrly_config::{load, load_str};
use pawrly_core::ConfigError;
use pawrly_secrets::StaticStore;

fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn global_variable_default_inlined() {
    let yaml = r#"
version: 1
variables:
  API_BASE:
    kind: variable
    default: https://api.example.com
sources:
  - name: gh
    kind: http
    config:
      base_url: ${var:API_BASE}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    assert_eq!(
        cfg.sources[0].config["base_url"].as_str(),
        Some("https://api.example.com")
    );
}

#[test]
fn typed_variable_defaults_inline_with_json_type() {
    let yaml = r#"
version: 1
variables:
  PAGE_SIZE:
    kind: variable
    type: number
    default: 100
  VERIFY_SSL:
    kind: variable
    type: boolean
    default: true
  REGION:
    kind: variable
    type: enum
    choices: [us, eu, ap]
    default: us
sources:
  - name: gh
    kind: http
    config:
      page_size: ${var:PAGE_SIZE}
      verify_ssl: ${var:VERIFY_SSL}
      region: ${var:REGION}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    let c = &cfg.sources[0].config;
    assert_eq!(c["page_size"], serde_json::json!(100));
    assert_eq!(c["verify_ssl"], serde_json::json!(true));
    assert_eq!(c["region"], serde_json::json!("us"));
}

#[test]
fn enum_default_outside_choices_rejected() {
    let yaml = r#"
version: 1
variables:
  REGION:
    kind: variable
    type: enum
    choices: [us, eu]
    default: ap
sources: []
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("choices")));
}

#[test]
fn optional_variable_resolves_to_null() {
    let yaml = r#"
version: 1
variables:
  TIMEOUT:
    kind: variable
    type: number
    required: false
    input: PAWRLY_UNSET_TIMEOUT_7B2
sources:
  - name: gh
    kind: http
    config:
      timeout: ${var:TIMEOUT}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    assert!(cfg.sources[0].config["timeout"].is_null());
}

#[test]
fn required_variable_unset_is_load_error() {
    let yaml = r#"
version: 1
variables:
  HOST:
    kind: variable
    input: PAWRLY_UNSET_HOST_7B2
sources:
  - name: gh
    kind: http
    config:
      host: ${var:HOST}
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { name, .. } if name == "HOST"));
}

#[test]
fn type_on_secret_rejected() {
    let yaml = r#"
version: 1
variables:
  API_TOKEN:
    kind: secret
    type: number
sources: []
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("type")));
}

#[test]
fn static_secret_resolves_from_store() {
    let yaml = r#"
version: 1
variables:
  API_TOKEN:
    kind: secret
sources:
  - name: gh
    kind: http
    config:
      token: ${var:API_TOKEN}
"#;
    let secrets = StaticStore::new();
    secrets.insert("API_TOKEN", "ghp_secret");
    let cfg = load_str(yaml, &secrets).unwrap();
    assert_eq!(cfg.sources[0].config["token"].as_str(), Some("ghp_secret"));
}

#[test]
fn source_local_variable_resolves_and_is_retained() {
    let yaml = r#"
version: 1
sources:
  - name: gh
    kind: http
    variables:
      LOCAL:
        kind: variable
        default: local-value
    config:
      x: ${var:LOCAL}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    assert_eq!(cfg.sources[0].config["x"].as_str(), Some("local-value"));
    assert!(cfg.sources[0].variables.contains_key("LOCAL"));
}

#[test]
fn source_local_shadows_global() {
    let yaml = r#"
version: 1
variables:
  T:
    kind: variable
    default: outer
sources:
  - name: a
    kind: http
    config:
      x: ${var:T}
  - name: b
    kind: http
    variables:
      T:
        kind: variable
        default: inner
    config:
      x: ${var:T}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    let a = cfg.sources.iter().find(|s| s.name == "a").unwrap();
    let b = cfg.sources.iter().find(|s| s.name == "b").unwrap();
    assert_eq!(a.config["x"].as_str(), Some("outer"));
    assert_eq!(b.config["x"].as_str(), Some("inner"));
}

#[test]
fn variable_is_not_visible_to_another_source() {
    let yaml = r#"
version: 1
sources:
  - name: a
    kind: http
    variables:
      LOCAL:
        kind: variable
        default: x
    config:
      v: ${var:LOCAL}
  - name: b
    kind: http
    config:
      v: ${var:LOCAL}
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(
        matches!(&err, ConfigError::Variable { name, .. } if name == "LOCAL"),
        "expected an isolation error for `b`, got {err:?}"
    );
}

#[test]
fn undeclared_reference_rejected() {
    let yaml = r#"
version: 1
sources:
  - name: gh
    kind: http
    config:
      x: ${var:NOPE}
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { name, .. } if name == "NOPE"));
}

#[test]
fn secret_with_default_rejected() {
    let yaml = r#"
version: 1
variables:
  BAD:
    kind: secret
    default: nope
sources: []
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("default")));
}

#[test]
fn credential_like_variable_rejected() {
    let yaml = r#"
version: 1
variables:
  API_TOKEN:
    kind: variable
sources: []
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("credential")));
}

#[test]
fn dynamic_oauth_reference_emits_binding_left_verbatim() {
    let yaml = r#"
version: 1
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
sources:
  - name: gh
    kind: http
    config:
      token: ${var:GH_TOKEN}
"#;
    let cfg = load_str(yaml, &StaticStore::new()).unwrap();
    assert_eq!(
        cfg.sources[0].config["token"].as_str(),
        Some("${var:GH_TOKEN}")
    );
    assert_eq!(cfg.sources[0].dynamic_vars.len(), 1);
    assert_eq!(cfg.sources[0].dynamic_vars[0].name, "GH_TOKEN");
    assert_eq!(cfg.sources[0].dynamic_vars[0].id, "root::GH_TOKEN");
    assert!(cfg.dynamic_specs().contains_key("root::GH_TOKEN"));
}

#[test]
fn malformed_oauth_missing_endpoints_rejected() {
    let yaml = r#"
version: 1
variables:
  SF_TOKEN:
    kind: secret
    oauth:
      grant:
        type: device_code
sources:
  - name: sf
    kind: http
    config:
      token: ${var:SF_TOKEN}
"#;
    let err = load_str(yaml, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { name, .. } if name == "SF_TOKEN"));
}

#[test]
fn fragment_scoped_variable_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let root = write(
        dir.path(),
        "pawrly.yaml",
        "version: 1\ninclude:\n  - ./frag.yaml\n",
    );
    write(
        dir.path(),
        "frag.yaml",
        r#"
variables:
  FRAG_VAR:
    kind: variable
    default: frag-value
sources:
  - name: s1
    kind: http
    config:
      x: ${var:FRAG_VAR}
"#,
    );
    let cfg = load(&root, &StaticStore::new()).unwrap_or_else(|e| panic!("load failed: {e}"));
    let s1 = cfg.sources.iter().find(|s| s.name == "s1").unwrap();
    assert_eq!(s1.config["x"].as_str(), Some("frag-value"));
}

#[test]
fn fragment_variable_not_visible_to_root_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let root = write(
        dir.path(),
        "pawrly.yaml",
        r#"
version: 1
include:
  - ./frag.yaml
sources:
  - name: root_src
    kind: http
    config:
      x: ${var:FRAG_VAR}
"#,
    );
    write(
        dir.path(),
        "frag.yaml",
        r#"
variables:
  FRAG_VAR:
    kind: variable
    default: frag-value
sources:
  - name: s1
    kind: http
    config:
      x: ${var:FRAG_VAR}
"#,
    );
    let err = load(&root, &StaticStore::new()).unwrap_err();
    assert!(matches!(err, ConfigError::Variable { name, .. } if name == "FRAG_VAR"));
}

#[test]
fn variable_default_can_use_secret_reference() {
    // Pass A resolves the `${secret:}` in the default before Pass B inlines the variable.
    let yaml = r#"
version: 1
variables:
  ENDPOINT:
    kind: variable
    default: ${secret:BASE}
sources:
  - name: gh
    kind: http
    config:
      base_url: ${var:ENDPOINT}
"#;
    let secrets = StaticStore::new();
    secrets.insert("BASE", "https://resolved.example.com");
    let cfg = load_str(yaml, &secrets).unwrap();
    assert_eq!(
        cfg.sources[0].config["base_url"].as_str(),
        Some("https://resolved.example.com")
    );
}

#[test]
fn source_static_vars_reports_unresolved_secret_for_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = write(
        dir.path(),
        "pawrly.yaml",
        r#"
version: 1
variables:
  API_BASE:
    kind: variable
    default: https://api.example.com
  API_TOKEN:
    kind: secret
    input: PAWRLY_TEST_UNSET_TOKEN_3C13DC9F
sources:
  - name: gh
    kind: http
    config:
      base_url: ${var:API_BASE}
      token: ${var:API_TOKEN}
"#,
    );

    let refs = pawrly_config::source_static_vars(&root, "gh", None).unwrap();
    let missing: Vec<_> = refs.iter().filter(|r| !r.resolves).collect();
    assert_eq!(missing.len(), 1, "got: {missing:?}");
    assert_eq!(missing[0].name, "API_TOKEN");
    assert_eq!(missing[0].kind, pawrly_config::VarKind::Secret);
    assert_eq!(missing[0].input_key, "PAWRLY_TEST_UNSET_TOKEN_3C13DC9F");
    assert_eq!(missing[0].var_id, "root::API_TOKEN");

    assert!(
        pawrly_config::source_static_vars(&root, "nope", None)
            .unwrap()
            .is_empty()
    );
}
