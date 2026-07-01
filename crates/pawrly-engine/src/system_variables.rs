//! The `system.variables` introspection table.

use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use datafusion::datasource::MemTable;

use secrecy::ExposeSecret as _;

use pawrly_config::{Config, VarKind, VariableDef};
use pawrly_secrets::{SecretStore, VariableValueStore};

struct Row {
    source: String,
    key: String,
    kind: &'static str,
    var_type: &'static str,
    value: Option<String>,
    default_value: Option<String>,
    description: Option<String>,
    required: bool,
    available: bool,
}

pub fn build_variables_table(
    cfg: &Config,
    tokens: &dyn VariableValueStore,
    secrets: &dyn SecretStore,
) -> Option<MemTable> {
    let rows = collect(cfg, tokens, secrets);
    if rows.is_empty() {
        return None;
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, true),
        Field::new("default_value", DataType::Utf8, true),
        Field::new("description", DataType::Utf8, true),
        Field::new("required", DataType::Boolean, false),
        Field::new("available", DataType::Boolean, false),
    ]));

    let str_col = |vals: Vec<Option<String>>| -> ArrayRef { Arc::new(StringArray::from(vals)) };
    let columns: Vec<ArrayRef> = vec![
        str_col(rows.iter().map(|r| Some(r.source.clone())).collect()),
        str_col(rows.iter().map(|r| Some(r.key.clone())).collect()),
        str_col(rows.iter().map(|r| Some(r.kind.to_string())).collect()),
        str_col(rows.iter().map(|r| Some(r.var_type.to_string())).collect()),
        str_col(rows.iter().map(|r| r.value.clone()).collect()),
        str_col(rows.iter().map(|r| r.default_value.clone()).collect()),
        str_col(rows.iter().map(|r| r.description.clone()).collect()),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.required).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.available).collect::<Vec<_>>(),
        )),
    ];

    let batch = RecordBatch::try_new(schema.clone(), columns).ok()?;
    MemTable::try_new(schema, vec![vec![batch]]).ok()
}

fn collect(cfg: &Config, tokens: &dyn VariableValueStore, secrets: &dyn SecretStore) -> Vec<Row> {
    let specs = cfg.dynamic_specs();

    let mut rows = Vec::new();
    let mut push = |source: String, scope_id: &str, name: &str, def: &VariableDef| {
        let dynamic = def.is_dynamic();
        let var_id = format!("{scope_id}::{name}");
        let stored = tokens
            .get(&pawrly_secrets::value_key(&var_id))
            .ok()
            .flatten()
            .map(|s| s.expose_secret().to_string());
        let has_literal = stored.is_some();
        let available = if dynamic {
            has_literal
                || match specs.get(&var_id) {
                    Some(spec) if !spec.is_interactive() => true,
                    Some(_) => tokens.get(&var_id).ok().flatten().is_some(),
                    None => false,
                }
        } else {
            let input = def.input_key().unwrap_or(name);
            !def.required
                || match def.kind {
                    VarKind::Secret => has_literal || secrets.get(input).ok().flatten().is_some(),
                    VarKind::Variable => {
                        has_literal || def.default.is_some() || std::env::var(input).is_ok()
                    }
                }
        };
        let default_value = def.default.as_ref().map(value_to_string);
        let value = match def.kind {
            VarKind::Secret => None,
            VarKind::Variable if dynamic => None,
            VarKind::Variable => stored.clone().or_else(|| default_value.clone()),
        };
        rows.push(Row {
            source,
            key: name.to_string(),
            kind: match def.kind {
                VarKind::Variable => "variable",
                VarKind::Secret => "secret",
            },
            var_type: def.var_type().as_str(),
            value,
            default_value,
            description: def.description.clone(),
            required: def.required && def.default.is_none(),
            available,
        });
    };

    for (name, def) in &cfg.variables {
        push("global".to_string(), "root", name, def);
    }
    for s in &cfg.sources {
        let scope_id = format!("source:{}", s.name);
        for (name, def) in &s.variables {
            push(s.name.clone(), &scope_id, name, def);
        }
    }
    rows
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use pawrly_secrets::{NoopTokenStore, StaticStore};

    fn cfg(yaml: &str) -> Config {
        pawrly_config::load_str(yaml, &StaticStore::new()).expect("parse")
    }

    fn available_of(rows: &[Row], key: &str) -> bool {
        rows.iter().find(|r| r.key == key).expect("row").available
    }

    #[test]
    fn static_secret_available_reflects_secret_store() {
        let c = cfg("version: 1\nvariables:\n  API_TOKEN: { kind: secret }\nsources: []\n");

        let store = StaticStore::new();
        store.insert("API_TOKEN", "shhh");
        let with = collect(&c, &NoopTokenStore, &store);
        assert!(
            available_of(&with, "API_TOKEN"),
            "found in the secret store"
        );

        let without = collect(&c, &NoopTokenStore, &StaticStore::new());
        assert!(!available_of(&without, "API_TOKEN"), "absent everywhere");
    }

    #[test]
    fn variable_with_default_is_available() {
        let c = cfg(
            "version: 1\nvariables:\n  API_BASE: { kind: variable, default: https://x }\nsources: []\n",
        );
        assert!(available_of(
            &collect(&c, &NoopTokenStore, &StaticStore::new()),
            "API_BASE"
        ));
    }

    #[test]
    fn static_secret_available_from_value_store_by_var_id() {
        use pawrly_secrets::{FileTokenStore, VariableValueStore as _};
        let c = cfg("version: 1\nvariables:\n  API_TOKEN: { kind: secret }\nsources: []\n");
        let dir = tempfile::tempdir().unwrap();
        let tokens = FileTokenStore::new(dir.path().join("v.json"));
        tokens
            .set(
                &pawrly_secrets::value_key("root::API_TOKEN"),
                &pawrly_secrets::Secret::from("v".to_string()),
            )
            .unwrap();
        let rows = collect(&c, &tokens, &StaticStore::new());
        assert!(
            available_of(&rows, "API_TOKEN"),
            "resolved from the value store"
        );
    }

    #[test]
    fn variable_stored_value_is_available_and_shown() {
        use pawrly_secrets::{FileTokenStore, VariableValueStore as _};
        let c = cfg("version: 1\nvariables:\n  REGION: { kind: variable }\nsources: []\n");
        let dir = tempfile::tempdir().unwrap();
        let tokens = FileTokenStore::new(dir.path().join("v.json"));
        tokens
            .set(
                &pawrly_secrets::value_key("root::REGION"),
                &pawrly_secrets::Secret::from("eu".to_string()),
            )
            .unwrap();
        let rows = collect(&c, &tokens, &StaticStore::new());
        let row = rows.iter().find(|r| r.key == "REGION").expect("row");
        assert!(row.available, "a stored value makes it available");
        assert_eq!(row.value.as_deref(), Some("eu"));
    }
}
