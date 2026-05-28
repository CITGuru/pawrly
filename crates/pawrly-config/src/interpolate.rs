//! `${secret:NAME}`, `${env:NAME}`, `${file:PATH}` interpolation.
//!
//! Walks an arbitrary `serde_json::Value` tree and substitutes any string
//! that is *exactly* a single reference. Mixed strings (e.g. `prefix-${env:X}`)
//! are intentionally rejected — values are atomic.

use std::path::Path;

use secrecy::ExposeSecret as _;
use serde_json::Value;

use pawrly_core::ConfigError;
use pawrly_secrets::SecretStore;

/// Recursively interpolate references in a JSON tree.
///
/// On any unresolved reference, returns the *first* error encountered.
/// (We accumulate all the others during validation, not here.)
pub fn resolve(value: &mut Value, secrets: &dyn SecretStore) -> Result<(), ConfigError> {
    walk(value, secrets)
}

fn walk(value: &mut Value, secrets: &dyn SecretStore) -> Result<(), ConfigError> {
    match value {
        Value::String(s) => {
            if let Some(replaced) = resolve_one(s, secrets)? {
                *s = replaced;
            }
            Ok(())
        }
        Value::Array(arr) => {
            for v in arr {
                walk(v, secrets)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                walk(v, secrets)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Resolve a single string. Returns `None` if it isn't a reference.
fn resolve_one(s: &str, secrets: &dyn SecretStore) -> Result<Option<String>, ConfigError> {
    let trimmed = s.trim();
    let Some(rest) = trimmed.strip_prefix("${") else {
        return Ok(None);
    };
    let Some(body) = rest.strip_suffix('}') else {
        return Ok(None);
    };
    let Some((prefix, target)) = body.split_once(':') else {
        return Ok(None);
    };

    match prefix {
        "secret" => match secrets
            .get(target)
            .map_err(|e| ConfigError::Io(e.to_string()))?
        {
            Some(v) => Ok(Some(v.expose_secret().to_string())),
            None => Err(ConfigError::UnresolvedSecret(target.to_string())),
        },
        "env" => match std::env::var(target) {
            Ok(v) => Ok(Some(v)),
            Err(_) => Err(ConfigError::UnresolvedEnv(target.to_string())),
        },
        "file" => {
            let path = expand_tilde(target);
            std::fs::read_to_string(&path)
                .map(|c| Some(c.trim().to_string()))
                .map_err(|e| ConfigError::ReadFile {
                    path: path.display().to_string(),
                    msg: e.to_string(),
                })
        }
        _ => Ok(None),
    }
}

/// Tilde-expand `~` and `~/` to `$HOME`. Returns the input unchanged if the
/// expansion can't be performed.
fn expand_tilde(input: &str) -> std::path::PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    if input == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).to_path_buf();
        }
    }
    Path::new(input).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_secrets::StaticStore;
    use serde_json::json;

    #[test]
    fn replaces_secret_reference() {
        let secrets = StaticStore::new();
        secrets.insert("FOO", "secret-value");
        let mut v = json!({
            "token": "${secret:FOO}",
            "nested": {
                "x": "${secret:FOO}",
                "literal": "no-touch"
            },
            "arr": ["${secret:FOO}", "raw"]
        });
        resolve(&mut v, &secrets).unwrap();
        assert_eq!(v["token"], json!("secret-value"));
        assert_eq!(v["nested"]["x"], json!("secret-value"));
        assert_eq!(v["nested"]["literal"], json!("no-touch"));
        assert_eq!(v["arr"][0], json!("secret-value"));
    }

    #[test]
    fn unresolved_secret_errors() {
        let secrets = StaticStore::new();
        let mut v = json!({"x": "${secret:MISSING}"});
        let err = resolve(&mut v, &secrets).unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedSecret(s) if s == "MISSING"));
    }

    #[test]
    fn ignores_non_references() {
        let secrets = StaticStore::new();
        let mut v = json!({
            "literal": "no $thing here",
            "wrong_prefix": "${other:X}",
            "no_colon": "${noColon}",
        });
        resolve(&mut v, &secrets).unwrap();
        assert_eq!(v["literal"], json!("no $thing here"));
        assert_eq!(v["wrong_prefix"], json!("${other:X}"));
    }
}
