//! Per-config validation rules.
//!
//! Returns *all* errors, not the first one, so users see every problem at once.

use pawrly_core::{ConfigError, ConfigErrors, SourceKind};

use crate::types::Config;

/// Run every validation rule and accumulate the results.
#[must_use]
pub fn validate(cfg: &Config) -> ConfigErrors {
    let mut errors = ConfigErrors::default();

    if cfg.version != 1 {
        errors.push(ConfigError::UnsupportedVersion(cfg.version));
    }

    let mut seen = std::collections::HashSet::new();
    for src in &cfg.sources {
        if !seen.insert(src.name.clone()) {
            errors.push(ConfigError::Source(
                src.name.clone(),
                "duplicate source name".to_string(),
            ));
        }
        validate_source(src, &mut errors);
    }

    errors
}

fn validate_source(src: &crate::types::SourceDef, errors: &mut ConfigErrors) {
    if src.name.is_empty() {
        errors.push(ConfigError::Source(
            "<unnamed>".to_string(),
            "source `name:` is required".into(),
        ));
    } else if !is_valid_identifier(&src.name) {
        errors.push(ConfigError::Source(
            src.name.clone(),
            "source name must be a valid SQL identifier".into(),
        ));
    }

    // `raw_table: true` only makes sense for HTTP-shaped sources.
    if src.raw_table && !src.kind.is_http_shaped() {
        errors.push(ConfigError::Source(
            src.name.clone(),
            format!(
                "`raw_table: true` is only valid for HTTP-shaped sources; \
                 `{}` is not HTTP-shaped",
                src.kind
            ),
        ));
    }

    // Per-table validation.
    let mut table_seen = std::collections::HashSet::new();
    for t in &src.tables {
        if !table_seen.insert(t.name.clone()) {
            errors.push(ConfigError::Table {
                source_name: src.name.clone(),
                table: t.name.clone(),
                msg: "duplicate table name".into(),
            });
        }
        if t.name.is_empty() || !is_valid_identifier(&t.name) {
            errors.push(ConfigError::Table {
                source_name: src.name.clone(),
                table: t.name.clone(),
                msg: "table name must be a valid SQL identifier".into(),
            });
        }
    }

    // Per-kind hooks (lightweight; not all source kinds are validated yet).
    match src.kind {
        SourceKind::File => {
            // Either top-level path glob or per-table paths required.
            let top_path = src.config.get("path").and_then(|v| v.as_str());
            let any_table_path = src.tables.iter().any(|t| t.body.get("path").is_some());
            if top_path.is_none() && !any_table_path {
                errors.push(ConfigError::Source(
                    src.name.clone(),
                    "`kind: file` requires either top-level `config.path` or per-table `path`"
                        .into(),
                ));
            }
        }
        SourceKind::Ai => {
            if src
                .config
                .get("provider")
                .and_then(|v| v.as_str())
                .is_none()
            {
                errors.push(ConfigError::Source(
                    src.name.clone(),
                    "`kind: ai` requires `config.provider`".into(),
                ));
            }
        }
        _ => {}
    }
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Config, SourceDef};

    fn cfg(sources: Vec<SourceDef>) -> Config {
        Config {
            version: 1,
            name: "default".into(),
            defaults: Default::default(),
            secrets: Vec::new(),
            include: Vec::new(),
            sources,
        }
    }

    fn src(name: &str, kind: SourceKind, config: serde_json::Value) -> SourceDef {
        SourceDef {
            name: name.into(),
            kind,
            description: None,
            from: None,
            config,
            cache: Default::default(),
            safety: None,
            tables: Vec::new(),
            raw_table: false,
            raw_table_safety: None,
        }
    }

    #[test]
    fn version_check() {
        let mut c = cfg(Vec::new());
        c.version = 2;
        assert!(!validate(&c).is_empty());
    }

    #[test]
    fn duplicate_source_names_caught() {
        let c = cfg(vec![
            src("gh", SourceKind::Github, serde_json::json!({"token": "x"})),
            src("gh", SourceKind::Github, serde_json::json!({"token": "y"})),
        ]);
        let errs = validate(&c);
        assert!(!errs.is_empty());
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("duplicate")))
        );
    }

    #[test]
    fn raw_table_on_non_http_rejected() {
        let mut s = src(
            "data",
            SourceKind::File,
            serde_json::json!({"path": "./data/*.parquet"}),
        );
        s.raw_table = true;
        let c = cfg(vec![s]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("raw_table")))
        );
    }

    #[test]
    fn file_source_requires_path() {
        let s = src("data", SourceKind::File, serde_json::json!({}));
        let c = cfg(vec![s]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("path")))
        );
    }

    #[test]
    fn ai_requires_provider() {
        let s = src("models", SourceKind::Ai, serde_json::json!({}));
        let c = cfg(vec![s]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("provider")))
        );
    }

    #[test]
    fn identifier_check() {
        assert!(is_valid_identifier("gh"));
        assert!(is_valid_identifier("_warehouse"));
        assert!(is_valid_identifier("a1_b2"));
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("1abc"));
        assert!(!is_valid_identifier("with-dash"));
    }
}
