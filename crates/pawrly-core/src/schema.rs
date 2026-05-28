//! Schema descriptors used across the engine, gRPC API, and frontends.

use arrow_schema::SchemaRef;
use serde::{Deserialize, Serialize};

use crate::model::SourceKind;

/// Fully-qualified table name as `<schema>.<table>` where `schema` is a
/// source name (Pawrly always lives in catalog `pawrly`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableName {
    pub schema: String,
    pub table: String,
}

impl TableName {
    /// Construct a new `TableName`.
    pub fn new(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            table: table.into(),
        }
    }

    /// Parse `"schema.table"` form. Returns `None` if the form is wrong.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (schema, table) = s.split_once('.')?;
        if schema.is_empty() || table.is_empty() {
            return None;
        }
        Some(Self::new(schema, table))
    }
}

impl std::fmt::Display for TableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.table)
    }
}

/// Per-column metadata exposed via the catalog and gRPC `describe_table`.
///
/// The Arrow type is the source of truth for engine-side typing; the
/// `data_type` string is its canonical display form for clients that
/// don't link Arrow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSpec {
    pub name: String,
    /// Arrow data type as a string (e.g. `"Int64"`, `"Decimal128(18, 2)"`).
    pub data_type: String,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// True when the engine can push a filter on this column down into the source.
    #[serde(default)]
    pub is_filter_pushable: bool,
    /// True when scans must include a filter on this column (safety).
    #[serde(default)]
    pub is_required_filter: bool,
}

/// Engine-side description of a table that hasn't yet been registered.
/// Used by `SourceBuilder` outputs and tests.
#[derive(Debug, Clone)]
pub struct TableSpec {
    pub name: String,
    pub schema: SchemaRef,
    pub primary_key: Option<Vec<String>>,
    pub description: Option<String>,
}

/// Optional filter applied to `EngineService::list_tables`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TableFilter {
    /// Limit to one source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Glob over table name (without schema prefix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_glob: Option<String>,
}

/// Lightweight info row about a registered table — what `list_tables` returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: TableName,
    pub kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_count_estimate: Option<u64>,
    #[serde(default)]
    pub cached: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_filters: Vec<String>,
}

/// Detailed description of a single table, returned by `describe_table`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDescription {
    pub table: TableInfo,
    pub columns: Vec<ColumnSpec>,
    /// Columns that the source can absorb as predicates pushed down.
    #[serde(default)]
    pub pushable_filter_columns: Vec<String>,
    /// Example queries (often populated from bundled specs).
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Compact catalog overview produced by `EngineService::schema_snapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub schemas: Vec<SchemaSummary>,
}

/// One schema in a `CatalogSnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSummary {
    pub name: String,
    pub kind: SourceKind,
    pub tables: Vec<TableSummary>,
}

/// One table in a `SchemaSummary` (compact form for agent grounding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSummary {
    pub name: String,
    /// Single-line `"col1 type, col2 type, ..."` form.
    pub columns: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_filters: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_name_parse_round_trip() {
        let n = TableName::parse("gh.pulls").unwrap();
        assert_eq!(n.schema, "gh");
        assert_eq!(n.table, "pulls");
        assert_eq!(format!("{n}"), "gh.pulls");
    }

    #[test]
    fn table_name_parse_rejects_malformed() {
        assert!(TableName::parse("nope").is_none());
        assert!(TableName::parse(".x").is_none());
        assert!(TableName::parse("x.").is_none());
    }
}
