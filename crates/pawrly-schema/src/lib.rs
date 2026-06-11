//! JSON-Schema primitives shared by pawrly's schema-driven sources.
//!
//! These map a JSON-Schema node to pawrly's column/param type strings, resolve
//! local `$ref`s, and locate the rows array inside a response envelope. They are
//! deliberately tolerant of real-world specs: unknown shapes degrade to `json`
//! rather than failing, and `type` is read as either a string or a 3.1-style
//! `["string", "null"]` array.

use serde_json::Value;

/// The first non-null JSON-Schema type, tolerating `type: [..]` arrays.
pub fn schema_type(schema: &Value) -> Option<String> {
    match schema.get("type") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_str)
            .find(|s| *s != "null")
            .map(str::to_string),
        _ => None,
    }
}

fn schema_format(schema: &Value) -> Option<&str> {
    schema.get("format").and_then(Value::as_str)
}

/// Whether a schema describes an object (explicit `type: object`, or untyped
/// with `properties`).
pub fn is_object(schema: &Value) -> bool {
    match schema_type(schema).as_deref() {
        Some("object") => true,
        None => schema.get("properties").is_some(),
        _ => false,
    }
}

/// Map a schema node to a pawrly **column** type string. Objects and arrays keep
/// their raw JSON (`json`); unknown/absent types also fall through to `json`.
pub fn column_type(schema: &Value) -> String {
    match schema_type(schema).as_deref() {
        Some("integer") => int_type(schema),
        Some("number") => number_type(schema),
        Some("boolean") => "bool".into(),
        Some("string") => match schema_format(schema) {
            Some("date") => "date".into(),
            Some("date-time") => "timestamp".into(),
            _ => "varchar".into(),
        },
        Some("object" | "array") => "json".into(),
        _ => "json".into(),
    }
}

/// Map a schema node to a pawrly **param** (scalar filter) type string. Anything
/// non-scalar falls through to `varchar`.
pub fn param_type(schema: &Value) -> String {
    match schema_type(schema).as_deref() {
        Some("integer") => int_type(schema),
        Some("number") => number_type(schema),
        Some("boolean") => "bool".into(),
        _ => "varchar".into(),
    }
}

fn int_type(schema: &Value) -> String {
    if schema_format(schema) == Some("int32") {
        "int".into()
    } else {
        "bigint".into()
    }
}

fn number_type(schema: &Value) -> String {
    if schema_format(schema) == Some("float") {
        "float".into()
    } else {
        "double".into()
    }
}

/// Follow a chain of local `#/...` `$ref`s within `doc`. External or
/// unresolvable refs return the reference node unchanged.
pub fn deref(doc: &Value, value: &Value) -> Value {
    let mut current = value;
    for _ in 0..32 {
        let Some(reference) = current.get("$ref").and_then(Value::as_str) else {
            return current.clone();
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return current.clone();
        };
        match doc.pointer(pointer) {
            Some(target) => current = target,
            None => return current.clone(),
        }
    }
    current.clone()
}

/// Whether a property name is response-envelope metadata rather than the rows
/// payload (skipped when looking for the single rows array).
pub fn is_metadata_field(name: &str) -> bool {
    matches!(
        name,
        "total_count" | "incomplete_results" | "has_more" | "next" | "previous"
    )
}

/// The single array property that holds rows within an object's `properties`: a
/// well-known name (`items`/`data`/`results`/`rows`) first, else the only
/// non-metadata array property. Returns the property name and its (dereferenced)
/// array schema.
pub fn rows_array(doc: &Value, props: &serde_json::Map<String, Value>) -> Option<(String, Value)> {
    for name in ["items", "data", "results", "rows"] {
        if let Some(prop) = props.get(name) {
            let prop = deref(doc, prop);
            if schema_type(&prop).as_deref() == Some("array") {
                return Some((name.to_string(), prop));
            }
        }
    }
    let mut arrays = props.iter().filter(|(name, prop)| {
        !is_metadata_field(name) && schema_type(&deref(doc, prop)).as_deref() == Some("array")
    });
    let first = arrays.next()?;
    if arrays.next().is_some() {
        return None;
    }
    Some((first.0.clone(), deref(doc, first.1)))
}

/// Deep-merge `patch` into `base`. Plain objects merge key-by-key; arrays and
/// scalars replace; an object carrying a `type` discriminator (a tagged union)
/// replaces wholesale rather than blending two variants. A `null` clears the key.
pub fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) if !p.contains_key("type") => {
            for (key, value) in p {
                deep_merge(b.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (b, p) => *b = p.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_merge_patches_plain_objects() {
        let mut base = json!({ "response": { "path": "$", "schema": [{ "name": "id" }] } });
        deep_merge(&mut base, &json!({ "response": { "path": "$.data" } }));
        assert_eq!(base["response"]["path"], json!("$.data"));
        assert_eq!(base["response"]["schema"], json!([{ "name": "id" }]));
    }

    #[test]
    fn deep_merge_replaces_tagged_unions_arrays_and_clears_on_null() {
        let mut base = json!({ "pagination": { "type": "a", "x": 1 }, "cols": [1, 2] });
        deep_merge(
            &mut base,
            &json!({ "pagination": { "type": "b" }, "cols": [9] }),
        );
        assert_eq!(base["pagination"], json!({ "type": "b" }));
        assert_eq!(base["cols"], json!([9]));
        deep_merge(&mut base, &json!({ "pagination": null }));
        assert_eq!(base["pagination"], Value::Null);
    }

    #[test]
    fn type_mapping_is_array_tolerant() {
        assert_eq!(
            column_type(&json!({ "type": ["string", "null"] })),
            "varchar"
        );
        assert_eq!(
            column_type(&json!({ "type": ["null", "integer"] })),
            "bigint"
        );
        assert_eq!(
            column_type(&json!({ "type": "string", "format": "date-time" })),
            "timestamp"
        );
        assert_eq!(
            column_type(&json!({ "type": "string", "format": "date" })),
            "date"
        );
        assert_eq!(
            column_type(&json!({ "type": "integer", "format": "int32" })),
            "int"
        );
        assert_eq!(
            column_type(&json!({ "type": "number", "format": "float" })),
            "float"
        );
        assert_eq!(column_type(&json!({ "type": "number" })), "double");
        assert_eq!(column_type(&json!({})), "json");
    }

    #[test]
    fn param_types_are_scalar_or_varchar() {
        assert_eq!(param_type(&json!({ "type": "integer" })), "bigint");
        assert_eq!(param_type(&json!({ "type": "boolean" })), "bool");
        assert_eq!(
            param_type(&json!({ "type": "string", "format": "date" })),
            "varchar"
        );
        assert_eq!(param_type(&json!({ "type": "object" })), "varchar");
    }

    #[test]
    fn deref_follows_local_refs() {
        let doc = json!({ "$defs": { "User": { "type": "object", "properties": { "id": {} } } } });
        let resolved = deref(&doc, &json!({ "$ref": "#/$defs/User" }));
        assert_eq!(schema_type(&resolved).as_deref(), Some("object"));
        // External ref is returned unchanged.
        assert!(
            deref(&doc, &json!({ "$ref": "https://x/y" }))
                .get("$ref")
                .is_some()
        );
    }

    #[test]
    fn rows_array_prefers_named_then_single() {
        let named = json!({
            "data": { "type": "array", "items": {} },
            "has_more": { "type": "boolean" }
        });
        assert_eq!(
            rows_array(&Value::Null, named.as_object().unwrap())
                .unwrap()
                .0,
            "data"
        );

        // Single non-metadata array wins when no named candidate matches.
        let single = json!({
            "payload": { "type": "array", "items": {} },
            "next": { "type": "array", "items": {} }
        });
        assert_eq!(
            rows_array(&Value::Null, single.as_object().unwrap())
                .unwrap()
                .0,
            "payload"
        );

        // Two payload arrays → ambiguous → none.
        let two = json!({
            "a": { "type": "array", "items": {} },
            "b": { "type": "array", "items": {} }
        });
        assert!(rows_array(&Value::Null, two.as_object().unwrap()).is_none());
    }
}
