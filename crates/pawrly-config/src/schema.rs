//! JSON Schema generation for `pawrly.yaml`. Used by `xtask schema` and by
//! editor language servers (yaml-language-server) for completion.

use schemars::{schema::RootSchema, schema_for};

use crate::types::Config;

/// Generate the JSON Schema for `pawrly.yaml` (Config root).
#[must_use]
pub fn json_schema() -> RootSchema {
    schema_for!(Config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_generates() {
        let s = json_schema();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"sources\""));
    }
}
