//! Shared `HttpSource` configuration shared between typed and raw tables.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, SchemaRef};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

/// Auth declaration. Supports bearer + api-key + basic.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthSpec {
    #[default]
    None,
    Bearer {
        token: String,
    },
    ApiKey {
        header: String,
        value: String,
    },
    Basic {
        username: String,
        password: String,
    },
}

/// Parameter declaration on a typed HTTP table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Name as declared in YAML (also the SQL column name when `source: param`).
    pub name: String,
    /// Type as a string (e.g. `varchar`, `int`).
    #[serde(default = "default_type")]
    pub r#type: String,
    /// Whether the parameter is required (= must appear as a filter).
    #[serde(default)]
    pub required: bool,
    /// Optional default value if the user didn't supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

fn default_type() -> String {
    "varchar".into()
}

/// Per-table declaration for an HTTP source.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpTableSpec {
    pub name: String,
    pub endpoint: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Response body shape (minimal).
    pub response: ResponseSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSpec {
    /// JSONPath to the row array. `$` means the response body itself is the array.
    #[serde(default = "default_response_path")]
    pub path: String,
    /// Declared columns. Each column has a name + Arrow type.
    pub schema: Vec<ResponseColumn>,
}

fn default_response_path() -> String {
    "$".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseColumn {
    pub name: String,
    pub r#type: String,
    /// Optional: pull from a JSONPath inside each row, or `param` to inject a request param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// In-memory shared state for an HTTP source.
pub struct HttpSource {
    pub name: String,
    pub base_url: url::Url,
    pub auth: AuthSpec,
    pub headers: HeaderMap,
    pub client: reqwest::Client,
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("name", &self.name)
            .field("base_url", &self.base_url.as_str())
            .finish()
    }
}

impl HttpSource {
    /// Build a `reqwest::Client` configured with reasonable defaults.
    pub fn build_client() -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent(format!("pawrly/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }
}

/// Build an Arrow `SchemaRef` from the declared response schema.
pub fn schema_for(table: &HttpTableSpec) -> SchemaRef {
    let fields: Vec<Field> = table
        .response
        .schema
        .iter()
        .map(|c| Field::new(&c.name, parse_arrow_type(&c.r#type), true))
        .collect();
    Arc::new(Schema::new(fields))
}

/// Map a YAML-declared type string to an Arrow `DataType`.
pub fn parse_arrow_type(s: &str) -> DataType {
    match s.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => DataType::Boolean,
        "int" | "int32" => DataType::Int32,
        "bigint" | "int64" | "long" => DataType::Int64,
        "float" | "float32" => DataType::Float32,
        "double" | "float64" => DataType::Float64,
        "varchar" | "string" | "text" => DataType::Utf8,
        _ => DataType::Utf8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arrow_types() {
        assert_eq!(parse_arrow_type("varchar"), DataType::Utf8);
        assert_eq!(parse_arrow_type("bigint"), DataType::Int64);
        assert_eq!(parse_arrow_type("int"), DataType::Int32);
        assert_eq!(parse_arrow_type("bool"), DataType::Boolean);
    }
}
