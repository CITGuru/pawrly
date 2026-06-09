//! Per-config validation rules.
//!
//! Returns *all* errors, not the first one, so users see every problem at once.

use pawrly_core::semantic::{DimensionType, MeasureAgg, SemanticModel, TimeGrain};
use pawrly_core::{ConfigError, ConfigErrors, SourceKind, TableName};

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
        if src.name == pawrly_core::MATERIALIZED_SCHEMA {
            errors.push(ConfigError::Source(
                src.name.clone(),
                "`materialized` is reserved for materialized tables".to_string(),
            ));
        }
        if !seen.insert(src.name.clone()) {
            errors.push(ConfigError::Source(
                src.name.clone(),
                "duplicate source name".to_string(),
            ));
        }
        validate_source(src, &mut errors);
    }

    if let Some(semantic) = &cfg.semantic {
        validate_semantic(&semantic.models, &seen, &mut errors);
    }

    errors
}

/// Validate the `semantic:` block. `source_names` is the set of configured
/// source names (each becomes a schema in the `pawrly` catalog).
///
/// Validating `required_predicates` and `Custom` measure SQL fully would need a
/// SQL expression parser; the compiler deliberately carries no DataFusion
/// dependency, so here we validate the structural subset (placeholder syntax,
/// non-empty SQL); a genuinely malformed expression still surfaces at query
/// time, and RLS params
/// are bound safely regardless (unbound params are refused as `UnboundParam`).
fn validate_semantic(
    models: &[SemanticModel],
    source_names: &std::collections::HashSet<String>,
    errors: &mut ConfigErrors,
) {
    let model_names: std::collections::HashSet<&str> =
        models.iter().map(|m| m.name.as_str()).collect();
    let mut model_seen = std::collections::HashSet::new();
    for model in models {
        let invalid = |msg: String| ConfigError::SemanticInvalid {
            model: model.name.clone(),
            msg,
        };

        if model.name.is_empty() || !is_valid_identifier(&model.name) {
            errors.push(invalid(
                "model `name:` must be a valid SQL identifier".into(),
            ));
        }
        if !model_seen.insert(model.name.clone()) {
            errors.push(invalid("duplicate model name".into()));
        }

        // Rule 1: `source:` parses as `schema.table` and the schema resolves
        // to a configured source.
        match TableName::parse(&model.source) {
            None => errors.push(invalid(format!(
                "`source: {}` must be in `source.table` form",
                model.source
            ))),
            Some(table) if !source_names.contains(&table.schema) => {
                errors.push(invalid(format!(
                    "`source: {}` references unknown source `{}`",
                    model.source, table.schema
                )));
            }
            Some(_) => {}
        }

        // Rule 3 + identifier hygiene for dimensions. Track the finest declared
        // grain per dimension for the pre-agg coarseness check (rule 5).
        let mut dim_seen = std::collections::HashSet::new();
        let mut finest_grain: std::collections::HashMap<&str, TimeGrain> =
            std::collections::HashMap::new();
        for dim in &model.dimensions {
            if dim.name.is_empty() || !is_valid_identifier(&dim.name) {
                errors.push(invalid(format!(
                    "dimension `{}` name must be a valid SQL identifier",
                    dim.name
                )));
            }
            if !dim_seen.insert(dim.name.clone()) {
                errors.push(invalid(format!("duplicate dimension `{}`", dim.name)));
            }
            // Rule 4: `grains:` is meaningful only on `type: time`.
            if !dim.time_grains.is_empty() && dim.data_type != DimensionType::Time {
                errors.push(invalid(format!(
                    "dimension `{}` declares `grains:` but is not `type: time`",
                    dim.name
                )));
            }
            if let Some(min) = dim.time_grains.iter().min_by_key(|g| g.rank()) {
                finest_grain.insert(dim.name.as_str(), *min);
            }
        }

        // Rule 3 + identifier hygiene for measures. Measure and dimension
        // names share the member namespace, so a name used by both is
        // ambiguous in a query member like `orders.foo`.
        let mut measure_seen = std::collections::HashSet::new();
        for measure in &model.measures {
            if measure.name.is_empty() || !is_valid_identifier(&measure.name) {
                errors.push(invalid(format!(
                    "measure `{}` name must be a valid SQL identifier",
                    measure.name
                )));
            }
            if !measure_seen.insert(measure.name.clone()) {
                errors.push(invalid(format!("duplicate measure `{}`", measure.name)));
            }
            if dim_seen.contains(&measure.name) {
                errors.push(invalid(format!(
                    "`{}` is used by both a dimension and a measure",
                    measure.name
                )));
            }
            // Rule 7: a `Custom` aggregate must carry non-empty SQL.
            if let MeasureAgg::Custom { sql } = &measure.agg {
                if sql.trim().is_empty() {
                    errors.push(invalid(format!(
                        "measure `{}` has a `custom` aggregate with empty `sql`",
                        measure.name
                    )));
                }
            }
        }

        // Rule 2: every relationship targets a known model; names are unique
        // and the join predicate is non-empty.
        let mut rel_seen = std::collections::HashSet::new();
        for rel in &model.relationships {
            if !rel_seen.insert(rel.name.clone()) {
                errors.push(invalid(format!("duplicate relationship `{}`", rel.name)));
            }
            if !model_names.contains(rel.target_model.as_str()) {
                errors.push(invalid(format!(
                    "relationship `{}` targets unknown model `{}`",
                    rel.name, rel.target_model
                )));
            }
            if rel.join_predicate.trim().is_empty() {
                errors.push(invalid(format!(
                    "relationship `{}` has an empty `on` join predicate",
                    rel.name
                )));
            }
        }

        // Rule 5: pre-agg dim/measure refs exist on this model; a pre-agg grain
        // must be no finer than the dimension's finest declared grain.
        let mut preagg_seen = std::collections::HashSet::new();
        for pre in &model.pre_aggregations {
            if !preagg_seen.insert(pre.name.clone()) {
                errors.push(invalid(format!("duplicate pre-aggregation `{}`", pre.name)));
            }
            for dim_ref in &pre.dimensions {
                let (name, grain) = split_member_grain(dim_ref);
                if !dim_seen.contains(name) {
                    errors.push(invalid(format!(
                        "pre-aggregation `{}` references unknown dimension `{}`",
                        pre.name, name
                    )));
                    continue;
                }
                if let Some(grain_str) = grain {
                    match TimeGrain::parse(grain_str) {
                        None => errors.push(invalid(format!(
                            "pre-aggregation `{}` uses invalid grain `{}` on `{}`",
                            pre.name, grain_str, name
                        ))),
                        Some(g) => {
                            if let Some(finest) = finest_grain.get(name) {
                                if g.rank() < finest.rank() {
                                    errors.push(invalid(format!(
                                        "pre-aggregation `{}` grain `{}` on `{}` is finer than \
                                         the dimension's finest declared grain `{}`",
                                        pre.name,
                                        grain_str,
                                        name,
                                        finest.as_str()
                                    )));
                                }
                            }
                        }
                    }
                }
            }
            for m in &pre.measures {
                if !measure_seen.contains(m) {
                    errors.push(invalid(format!(
                        "pre-aggregation `{}` references unknown measure `{}`",
                        pre.name, m
                    )));
                }
            }
            if let Some(part) = &pre.partition_by {
                let (name, _) = split_member_grain(part);
                if !dim_seen.contains(name) {
                    errors.push(invalid(format!(
                        "pre-aggregation `{}` partitions by unknown dimension `{}`",
                        pre.name, name
                    )));
                }
            }
        }

        // Rule 6: safety guard rails.
        if let Some(safety) = &model.safety {
            for col in &safety.require_filters_on {
                if !dim_seen.contains(col) {
                    errors.push(invalid(format!(
                        "`safety.require_filters_on` references unknown dimension `{col}`"
                    )));
                }
            }
            for pred in &safety.required_predicates {
                if let Err(msg) = check_predicate_params(pred) {
                    errors.push(invalid(format!(
                        "`safety.required_predicates` entry `{pred}`: {msg}"
                    )));
                }
            }
        }

        // Rule 8: segment names are unique identifiers, carry at least one
        // filter, and any filter that targets this model names a known member.
        let mut segment_seen = std::collections::HashSet::new();
        for seg in &model.segments {
            if seg.name.is_empty() || !is_valid_identifier(&seg.name) {
                errors.push(invalid(format!(
                    "segment `{}` name must be a valid SQL identifier",
                    seg.name
                )));
            }
            if !segment_seen.insert(seg.name.clone()) {
                errors.push(invalid(format!("duplicate segment `{}`", seg.name)));
            }
            if seg.filters.is_empty() {
                errors.push(invalid(format!("segment `{}` has no filters", seg.name)));
            }
            for f in &seg.filters {
                let (member_model, field) = match f.member.split_once('.') {
                    Some((m, rest)) => (m, rest.split('.').next().unwrap_or(rest)),
                    None => {
                        errors.push(invalid(format!(
                            "segment `{}` filter member `{}` must be `model.field`",
                            seg.name, f.member
                        )));
                        continue;
                    }
                };
                // Only self-references are checked here; cross-model members are
                // resolved against the catalog at query time.
                if member_model == model.name
                    && !dim_seen.contains(field)
                    && !measure_seen.contains(field)
                {
                    errors.push(invalid(format!(
                        "segment `{}` references unknown member `{}`",
                        seg.name, f.member
                    )));
                }
            }
        }
    }
}

/// Split a possibly-grained member into `(name, grain)`: `"order_date.day"`
/// → `("order_date", Some("day"))`, `"status"` → `("status", None)`.
fn split_member_grain(s: &str) -> (&str, Option<&str>) {
    match s.split_once('.') {
        Some((name, grain)) => (name, Some(grain)),
        None => (s, None),
    }
}

/// Structural check of `${param:NAME}` placeholders in a required predicate:
/// every placeholder must be terminated and name a valid identifier.
fn check_predicate_params(pred: &str) -> Result<(), String> {
    if pred.trim().is_empty() {
        return Err("predicate is empty".into());
    }
    const OPEN: &str = "${param:";
    let mut rest = pred;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            return Err("unterminated `${param:...}` placeholder".into());
        };
        let name = &after[..end];
        if name.is_empty() || !is_valid_identifier(name) {
            return Err(format!("`${{param:{name}}}` is not a valid identifier"));
        }
        rest = &after[end + 1..];
    }
    Ok(())
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
            // Object-store `file` (a `config.storage` block) reads remote URLs
            // via DuckDB and requires explicit per-table paths; local `file`
            // accepts a top-level glob or per-table paths.
            if let Some(storage) = src.config.get("storage") {
                let ty = storage.get("type").and_then(|v| v.as_str());
                if !matches!(ty, Some("s3" | "gcs" | "azure")) {
                    errors.push(ConfigError::Source(
                        src.name.clone(),
                        "`config.storage.type` must be one of `s3`, `gcs`, `azure`".into(),
                    ));
                }
                let any_table_path = src.tables.iter().any(|t| t.body.get("path").is_some());
                if !any_table_path {
                    errors.push(ConfigError::Source(
                        src.name.clone(),
                        "object-store `kind: file` requires at least one `tables[]` entry with a `path`"
                            .into(),
                    ));
                }
            } else {
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
        }
        SourceKind::Duckdb if src.config.get("path").and_then(|v| v.as_str()).is_none() => {
            errors.push(ConfigError::Source(
                src.name.clone(),
                "`kind: duckdb` requires `config.path` (a .duckdb file)".into(),
            ));
        }
        SourceKind::Ducklake if src.config.get("catalog").and_then(|v| v.as_str()).is_none() => {
            errors.push(ConfigError::Source(
                src.name.clone(),
                "`kind: ducklake` requires `config.catalog`".into(),
            ));
        }
        SourceKind::Postgres | SourceKind::Mysql => {
            let has_dsn = src.config.get("dsn").and_then(|v| v.as_str()).is_some();
            let has_host = src.config.get("host").and_then(|v| v.as_str()).is_some();
            let has_db = src
                .config
                .get("database")
                .or_else(|| src.config.get("dbname"))
                .and_then(|v| v.as_str())
                .is_some();
            if !(has_dsn || (has_host && has_db)) {
                errors.push(ConfigError::Source(
                    src.name.clone(),
                    format!(
                        "`kind: {}` requires `config.dsn` or both `config.host` and `config.database`",
                        src.kind
                    ),
                ));
            }
        }
        SourceKind::Snowflake => {
            for key in ["account", "user", "password"] {
                if src.config.get(key).and_then(|v| v.as_str()).is_none() {
                    errors.push(ConfigError::Source(
                        src.name.clone(),
                        format!("`kind: snowflake` requires `config.{key}`"),
                    ));
                }
            }
        }
        SourceKind::Iceberg | SourceKind::Delta => {
            if src.tables.is_empty() {
                errors.push(ConfigError::Source(
                    src.name.clone(),
                    format!(
                        "`kind: {}` requires at least one `tables[]` entry",
                        src.kind
                    ),
                ));
            }
            for t in &src.tables {
                let has_loc = t
                    .body
                    .get("path")
                    .or_else(|| t.body.get("location"))
                    .and_then(|v| v.as_str())
                    .is_some();
                if !has_loc {
                    errors.push(ConfigError::Table {
                        source_name: src.name.clone(),
                        table: t.name.clone(),
                        msg: "requires a `path` or `location` in its config".into(),
                    });
                }
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
            semantic: None,
        }
    }

    fn src(name: &str, kind: SourceKind, config: serde_json::Value) -> SourceDef {
        SourceDef {
            name: name.into(),
            kind,
            description: None,
            wiki: None,
            examples: Vec::new(),
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
            src(
                "gh",
                SourceKind::Http,
                serde_json::json!({"base_url": "https://x"}),
            ),
            src(
                "gh",
                SourceKind::Http,
                serde_json::json!({"base_url": "https://y"}),
            ),
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
    fn materialized_source_name_is_reserved() {
        let c = cfg(vec![src(
            "materialized",
            SourceKind::File,
            serde_json::json!({"path": "./data/*.parquet"}),
        )]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("reserved"))),
            "a source named `materialized` must be rejected"
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
    fn duckdb_requires_path() {
        let s = src("local", SourceKind::Duckdb, serde_json::json!({}));
        let c = cfg(vec![s]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("path")))
        );
    }

    #[test]
    fn object_store_file_requires_storage_type_and_table() {
        // A `file` source with a storage block but a bad type and no tables.
        let mut s = src(
            "lake",
            SourceKind::File,
            serde_json::json!({"storage": {"type": "bogus"}}),
        );
        s.tables = Vec::new();
        let c = cfg(vec![s]);
        let errs = validate(&c);
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("storage.type")))
        );
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ConfigError::Source(_, msg) if msg.contains("tables")))
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

    mod semantic {
        use super::*;
        use crate::types::SemanticConfig;
        use pawrly_core::semantic::{
            Dimension, DimensionType, Measure, MeasureAgg, SemanticModel, TimeGrain,
        };

        fn dim(name: &str, ty: DimensionType, grains: Vec<TimeGrain>) -> Dimension {
            Dimension {
                name: name.into(),
                expr: name.into(),
                data_type: ty,
                time_grains: grains,
                description: None,
            }
        }

        fn measure(name: &str) -> Measure {
            Measure {
                name: name.into(),
                agg: MeasureAgg::Sum,
                expr: "total".into(),
                filters: Vec::new(),
                format: None,
                description: None,
            }
        }

        fn model(name: &str, source: &str) -> SemanticModel {
            SemanticModel {
                name: name.into(),
                description: None,
                source: source.into(),
                primary_key: vec!["id".into()],
                dimensions: vec![dim("status", DimensionType::String, vec![])],
                measures: vec![measure("revenue")],
                relationships: vec![],
                pre_aggregations: vec![],
                segments: vec![],
                safety: None,
            }
        }

        /// A config with one `warehouse` source and the given semantic models.
        fn cfg_with(models: Vec<SemanticModel>) -> Config {
            let mut c = cfg(vec![src(
                "warehouse",
                SourceKind::Http,
                serde_json::json!({"base_url": "https://x"}),
            )]);
            c.semantic = Some(SemanticConfig {
                include: Vec::new(),
                models,
            });
            c
        }

        fn has_semantic_err(c: &Config, needle: &str) -> bool {
            validate(c).0.iter().any(
                |e| matches!(e, ConfigError::SemanticInvalid { msg, .. } if msg.contains(needle)),
            )
        }

        #[test]
        fn valid_model_passes() {
            let c = cfg_with(vec![model("orders", "warehouse.orders")]);
            assert!(
                !validate(&c)
                    .0
                    .iter()
                    .any(|e| matches!(e, ConfigError::SemanticInvalid { .. })),
                "{:?}",
                validate(&c).0
            );
        }

        #[test]
        fn unknown_source_rejected() {
            let c = cfg_with(vec![model("orders", "nope.orders")]);
            assert!(has_semantic_err(&c, "unknown source"));
        }

        #[test]
        fn malformed_source_rejected() {
            let c = cfg_with(vec![model("orders", "warehouse_orders")]);
            assert!(has_semantic_err(&c, "source.table"));
        }

        #[test]
        fn duplicate_model_rejected() {
            let c = cfg_with(vec![
                model("orders", "warehouse.orders"),
                model("orders", "warehouse.orders"),
            ]);
            assert!(has_semantic_err(&c, "duplicate model"));
        }

        #[test]
        fn duplicate_dimension_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.dimensions
                .push(dim("status", DimensionType::String, vec![]));
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "duplicate dimension"));
        }

        #[test]
        fn duplicate_measure_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.measures.push(measure("revenue"));
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "duplicate measure"));
        }

        #[test]
        fn grains_on_non_time_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.dimensions
                .push(dim("country", DimensionType::String, vec![TimeGrain::Day]));
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "not `type: time`"));
        }

        #[test]
        fn grains_on_time_allowed() {
            let mut m = model("orders", "warehouse.orders");
            m.dimensions.push(dim(
                "ordered_at",
                DimensionType::Time,
                vec![TimeGrain::Day, TimeGrain::Month],
            ));
            let c = cfg_with(vec![m]);
            assert!(!has_semantic_err(&c, "not `type: time`"));
        }

        #[test]
        fn dim_measure_name_collision_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.measures.push(measure("status")); // collides with the `status` dim
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "both a dimension and a measure"));
        }

        #[test]
        fn bad_model_name_rejected() {
            let c = cfg_with(vec![model("with-dash", "warehouse.orders")]);
            assert!(has_semantic_err(&c, "valid SQL identifier"));
        }

        // ---- rule 2: relationships ----

        use pawrly_core::safety::SafetyPolicy;
        use pawrly_core::semantic::{PreAggregation, Relationship, RelationshipKind};

        fn rel(name: &str, target: &str) -> Relationship {
            Relationship {
                name: name.into(),
                kind: RelationshipKind::ManyToOne,
                target_model: target.into(),
                join_predicate: "this.cid = customers.id".into(),
            }
        }

        #[test]
        fn relationship_unknown_target_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.relationships = vec![rel("customer", "customers")]; // no such model
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "targets unknown model"));
        }

        #[test]
        fn relationship_to_known_model_passes() {
            let mut orders = model("orders", "warehouse.orders");
            orders.relationships = vec![rel("customer", "customers")];
            let customers = model("customers", "warehouse.customers");
            let c = cfg_with(vec![orders, customers]);
            assert!(!has_semantic_err(&c, "targets unknown model"));
        }

        #[test]
        fn relationship_empty_predicate_rejected() {
            let mut m = model("orders", "warehouse.orders");
            let mut r = rel("customer", "orders");
            r.join_predicate = "  ".into();
            m.relationships = vec![r];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "empty `on`"));
        }

        // ---- rule 5: pre-aggregations ----

        fn preagg(name: &str, dims: &[&str], measures: &[&str]) -> PreAggregation {
            PreAggregation {
                name: name.into(),
                dimensions: dims.iter().map(|s| (*s).into()).collect(),
                measures: measures.iter().map(|s| (*s).into()).collect(),
                refresh: None,
                partition_by: None,
            }
        }

        #[test]
        fn preagg_unknown_measure_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.pre_aggregations = vec![preagg("daily", &["status"], &["nope"])];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "unknown measure"));
        }

        #[test]
        fn preagg_unknown_dimension_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.pre_aggregations = vec![preagg("daily", &["country"], &["revenue"])];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "unknown dimension"));
        }

        #[test]
        fn preagg_grain_finer_than_declared_rejected() {
            let mut m = model("orders", "warehouse.orders");
            // order_date supports month (and coarser); a daily rollup is finer.
            m.dimensions.push(dim(
                "order_date",
                DimensionType::Time,
                vec![TimeGrain::Month, TimeGrain::Year],
            ));
            m.pre_aggregations = vec![preagg("d", &["order_date.day"], &["revenue"])];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "finer than"));
        }

        #[test]
        fn preagg_valid_passes() {
            let mut m = model("orders", "warehouse.orders");
            m.dimensions.push(dim(
                "order_date",
                DimensionType::Time,
                vec![TimeGrain::Day, TimeGrain::Month],
            ));
            m.pre_aggregations = vec![preagg(
                "monthly",
                &["order_date.month", "status"],
                &["revenue"],
            )];
            let c = cfg_with(vec![m]);
            assert!(
                !validate(&c)
                    .0
                    .iter()
                    .any(|e| matches!(e, ConfigError::SemanticInvalid { .. })),
                "{:?}",
                validate(&c).0
            );
        }

        // ---- rule 6: safety ----

        #[test]
        fn safety_require_filter_unknown_dim_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.safety = Some(SafetyPolicy {
                require_filters_on: vec!["nope".into()],
                ..Default::default()
            });
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "require_filters_on"));
        }

        #[test]
        fn safety_predicate_bad_placeholder_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.safety = Some(SafetyPolicy {
                required_predicates: vec!["tenant_id = ${param:tenant".into()], // unterminated
                ..Default::default()
            });
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "unterminated"));
        }

        #[test]
        fn safety_valid_predicate_passes() {
            let mut m = model("orders", "warehouse.orders");
            m.safety = Some(SafetyPolicy {
                require_filters_on: vec!["status".into()],
                required_predicates: vec!["tenant_id = ${param:tenant_id}".into()],
                ..Default::default()
            });
            let c = cfg_with(vec![m]);
            assert!(
                !validate(&c)
                    .0
                    .iter()
                    .any(|e| matches!(e, ConfigError::SemanticInvalid { .. })),
                "{:?}",
                validate(&c).0
            );
        }

        // ---- rule 7: custom-SQL measures ----

        // ---- rule 8: segments ----

        use pawrly_core::semantic::{FilterOp, Segment, SemanticFilter};

        fn segment(name: &str, member: &str) -> Segment {
            Segment {
                name: name.into(),
                description: None,
                filters: vec![SemanticFilter {
                    member: member.into(),
                    op: FilterOp::Equals,
                    values: vec!["paid".into()],
                }],
            }
        }

        #[test]
        fn segment_valid_passes() {
            let mut m = model("orders", "warehouse.orders");
            m.segments = vec![segment("high_value", "orders.status")];
            let c = cfg_with(vec![m]);
            assert!(
                !validate(&c)
                    .0
                    .iter()
                    .any(|e| matches!(e, ConfigError::SemanticInvalid { .. })),
                "{:?}",
                validate(&c).0
            );
        }

        #[test]
        fn segment_unknown_self_member_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.segments = vec![segment("bad", "orders.nope")];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "unknown member"));
        }

        #[test]
        fn segment_empty_filters_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.segments = vec![Segment {
                name: "empty".into(),
                description: None,
                filters: vec![],
            }];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "no filters"));
        }

        #[test]
        fn segment_duplicate_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.segments = vec![
                segment("dup", "orders.status"),
                segment("dup", "orders.status"),
            ];
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "duplicate segment"));
        }

        #[test]
        fn custom_measure_empty_sql_rejected() {
            let mut m = model("orders", "warehouse.orders");
            m.measures.push(Measure {
                name: "ratio".into(),
                agg: MeasureAgg::Custom { sql: "   ".into() },
                expr: "a".into(),
                filters: vec![],
                format: None,
                description: None,
            });
            let c = cfg_with(vec![m]);
            assert!(has_semantic_err(&c, "empty `sql`"));
        }
    }
}
