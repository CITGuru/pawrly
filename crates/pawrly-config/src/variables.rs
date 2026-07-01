//! Declared, typed, scoped source variables: `variables:` blocks and
//! `${var:NAME}` references.

use std::collections::{BTreeMap, HashSet};

use secrecy::ExposeSecret as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pawrly_core::{
    ConfigError, DynamicVarBinding, DynamicVarSpec, Endpoints, PortMode, TokenTransport, VarId,
};
use pawrly_secrets::{SecretStore, VariableValueStore};

const RESERVED_PREFIX: &str = "__pawrly";

const CREDENTIAL_TERMS: [&str; 11] = [
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "apikey",
    "accesskey",
    "privatekey",
    "bearer",
    "credential",
    "clientsecret",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VarKind {
    Variable,
    Secret,
}

/// Underlying data type of a non-secret variable's value.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VarType {
    #[default]
    String,
    Integer,
    Number,
    Boolean,
    Enum,
}

impl VarType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VarType::String => "string",
            VarType::Integer => "integer",
            VarType::Number => "number",
            VarType::Boolean => "boolean",
            VarType::Enum => "enum",
        }
    }

    /// Whether an already-typed JSON value satisfies this type (`Enum` checked separately).
    fn accepts(self, v: &Value) -> bool {
        match self {
            VarType::String => v.is_string(),
            VarType::Integer => v.is_i64() || v.is_u64(),
            VarType::Number => v.is_number(),
            VarType::Boolean => v.is_boolean(),
            VarType::Enum => true,
        }
    }
}

fn default_required() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde attribute signature"
)]
fn is_true(b: &bool) -> bool {
    *b
}

/// One `variables:` entry.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariableDef {
    pub kind: VarKind,

    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<VarType>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Value>,

    #[serde(default = "default_required", skip_serializing_if = "is_true")]
    pub required: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<CredentialMethod>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

/// One way to collect a variable's value.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialMethod {
    Input(InputMethod),
    Oauth(Value),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InputMethod {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl VariableDef {
    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        self.oauth.is_some()
            || self
                .methods
                .iter()
                .any(|m| matches!(m, CredentialMethod::Oauth(_)))
    }

    /// True when the variable offers a static `input` (paste) collection method.
    #[must_use]
    pub fn has_input_method(&self) -> bool {
        self.input.is_some()
            || self
                .methods
                .iter()
                .any(|m| matches!(m, CredentialMethod::Input(_)))
    }

    #[must_use]
    pub fn var_type(&self) -> VarType {
        self.r#type.unwrap_or_default()
    }

    /// Validate a raw string value against the declared `type`/`choices`, returning the typed value.
    pub fn coerce(&self, raw: &str) -> Result<Value, String> {
        coerce_str("<value>", self.var_type(), &self.choices, raw).map_err(|e| match e {
            ConfigError::Variable { msg, .. } => msg,
            other => other.to_string(),
        })
    }

    fn has_shorthand(&self) -> bool {
        self.oauth.is_some() || self.input.is_some()
    }

    #[must_use]
    pub fn input_key(&self) -> Option<&str> {
        if let Some(key) = self.input.as_deref() {
            return Some(key);
        }
        self.methods.iter().find_map(|m| match m {
            CredentialMethod::Input(im) => im.input.as_deref(),
            CredentialMethod::Oauth(_) => None,
        })
    }

    fn oauth_body(&self) -> Option<&Value> {
        if let Some(v) = self.oauth.as_ref() {
            return Some(v);
        }
        self.methods.iter().find_map(|m| match m {
            CredentialMethod::Oauth(v) => Some(v),
            CredentialMethod::Input(_) => None,
        })
    }
}

/// A declaration plus its declaring scope; `scope_id` makes each `VarId` scope-unique (§7.1).
#[derive(Debug, Clone)]
pub struct ScopedVar {
    pub def: VariableDef,
    pub scope_id: String,
}

/// A source's merged variable scope (inner scope shadows outer).
pub type VariableScope = BTreeMap<String, ScopedVar>;

/// Parse a `variables:` block into a validated scope (Pass A runs first).
pub(crate) fn parse_block(block: &Value, scope_id: &str) -> Result<VariableScope, ConfigError> {
    let obj = block.as_object().ok_or_else(|| ConfigError::Variable {
        name: "<block>".to_string(),
        msg: "`variables:` must be a mapping of NAME to a definition".to_string(),
    })?;
    let mut out = VariableScope::new();
    for (name, raw) in obj {
        let def: VariableDef =
            serde_json::from_value(raw.clone()).map_err(|e| ConfigError::Variable {
                name: name.clone(),
                msg: e.to_string(),
            })?;
        validate_def(name, &def)?;
        out.insert(
            name.clone(),
            ScopedVar {
                def,
                scope_id: scope_id.to_string(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn merge_into(base: &mut VariableScope, overlay: &VariableScope) {
    for (k, scoped) in overlay {
        base.insert(k.clone(), scoped.clone());
    }
}

fn validate_def(name: &str, def: &VariableDef) -> Result<(), ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };

    if name.starts_with(RESERVED_PREFIX) {
        return Err(err(format!("name may not start with `{RESERVED_PREFIX}`")));
    }
    if !def.methods.is_empty() && def.has_shorthand() {
        return Err(err(
            "use either `methods:` or the `oauth`/`input` shorthand, not both".to_string(),
        ));
    }

    match def.kind {
        VarKind::Secret => {
            if def.default.is_some() {
                return Err(err(
                    "`default` is not allowed on a `kind: secret`".to_string()
                ));
            }
            if def.r#type.is_some() {
                return Err(err(
                    "`type` is only valid on a `kind: variable`; secrets are always opaque strings"
                        .to_string(),
                ));
            }
            if !def.choices.is_empty() {
                return Err(err(
                    "`choices` is only valid on a `kind: variable` with `type: enum`".to_string(),
                ));
            }
        }
        VarKind::Variable => {
            if is_credential_like(name) {
                return Err(err(
                    "name looks like a credential; declare it as `kind: secret`".to_string(),
                ));
            }
            if def.is_dynamic() {
                return Err(err(
                    "a `kind: variable` may not use an `oauth` credential method (only `kind: secret` can)"
                        .to_string(),
                ));
            }
            validate_var_type(name, def)?;
        }
    }
    Ok(())
}

/// Validate a non-secret variable's `type`/`choices`.
fn validate_var_type(name: &str, def: &VariableDef) -> Result<(), ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };
    match def.var_type() {
        VarType::Enum => {
            if def.choices.is_empty() {
                return Err(err(
                    "`type: enum` requires a non-empty `choices` list".to_string()
                ));
            }
            if let Some(d) = &def.default
                && !def.choices.contains(d)
            {
                return Err(err(format!(
                    "`default` `{}` is not one of the declared `choices`",
                    value_to_string(d)
                )));
            }
        }
        other => {
            if !def.choices.is_empty() {
                return Err(err("`choices` is only valid with `type: enum`".to_string()));
            }
            if let Some(d) = &def.default
                && !other.accepts(d)
            {
                return Err(err(format!(
                    "`default` `{}` does not match `type: {}`",
                    value_to_string(d),
                    other.as_str()
                )));
            }
        }
    }
    Ok(())
}

/// True if `name` is credential-shaped (and so must be a secret).
fn is_credential_like(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    CREDENTIAL_TERMS
        .iter()
        .any(|term| normalized.contains(term))
}

/// Pass B: resolve every `${var:NAME}` against `scope`, inlining static refs and returning dynamic ones as bindings.
pub(crate) fn resolve_refs(
    source: &mut Value,
    scope: &VariableScope,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
) -> Result<Vec<DynamicVarBinding>, ConfigError> {
    let mut bindings = Vec::new();
    let mut seen: HashSet<VarId> = HashSet::new();
    if let Value::Object(map) = source {
        for (key, child) in map.iter_mut() {
            if key == "variables" {
                continue;
            }
            walk(child, scope, secrets, vars, &mut bindings, &mut seen)?;
        }
    } else {
        walk(source, scope, secrets, vars, &mut bindings, &mut seen)?;
    }
    Ok(bindings)
}

fn walk(
    value: &mut Value,
    scope: &VariableScope,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
    bindings: &mut Vec<DynamicVarBinding>,
    seen: &mut HashSet<VarId>,
) -> Result<(), ConfigError> {
    match value {
        Value::String(s) => {
            // Own the name so the borrow on `s` ends before we reassign `*value`.
            let Some(name) = parse_ref(s).map(str::to_string) else {
                return Ok(());
            };
            let scoped = scope.get(&name).ok_or_else(|| ConfigError::Variable {
                name: name.clone(),
                msg: "referenced via `${var:…}` but not declared in this source's scope"
                    .to_string(),
            })?;
            if scoped.def.is_dynamic() {
                let id: VarId = format!("{}::{}", scoped.scope_id, name);
                // A stored literal overrides the OAuth flow: inline it and emit no binding.
                if let Some(lit) = stored_literal(vars, &id)? {
                    *value = Value::String(lit);
                } else if seen.insert(id.clone()) {
                    let spec = build_dynamic_spec(&name, &scoped.def, secrets)?;
                    bindings.push(DynamicVarBinding { name, id, spec });
                }
            } else {
                *value = resolve_value(&name, &scoped.def, secrets, vars, &scoped.scope_id)?;
            }
            Ok(())
        }
        Value::Array(arr) => {
            for v in arr {
                walk(v, scope, secrets, vars, bindings, seen)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                walk(v, scope, secrets, vars, bindings, seen)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// One referenced static `${var:NAME}` variable, its `VarId`, and whether it resolves now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticVarRef {
    pub name: String,
    pub var_id: VarId,
    pub kind: VarKind,
    pub input_key: String,
    pub resolves: bool,
}

/// Read-only counterpart to [`resolve_refs`]: report every static `${var:}` ref and its resolution state.
#[must_use]
pub(crate) fn collect_static_refs(
    source: &Value,
    scope: &VariableScope,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
) -> Vec<StaticVarRef> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Value::Object(map) = source {
        for (key, child) in map {
            if key == "variables" {
                continue;
            }
            collect_refs(child, scope, secrets, vars, &mut out, &mut seen);
        }
    } else {
        collect_refs(source, scope, secrets, vars, &mut out, &mut seen);
    }
    out
}

fn collect_refs(
    value: &Value,
    scope: &VariableScope,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
    out: &mut Vec<StaticVarRef>,
    seen: &mut HashSet<String>,
) {
    match value {
        Value::String(s) => {
            if let Some(name) = parse_ref(s)
                && let Some(scoped) = scope.get(name)
                && !scoped.def.is_dynamic()
                && seen.insert(name.to_string())
            {
                let resolves =
                    resolve_value(name, &scoped.def, secrets, vars, &scoped.scope_id).is_ok();
                out.push(StaticVarRef {
                    name: name.to_string(),
                    var_id: format!("{}::{}", scoped.scope_id, name),
                    kind: scoped.def.kind,
                    input_key: scoped.def.input_key().unwrap_or(name).to_string(),
                    resolves,
                });
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_refs(v, scope, secrets, vars, out, seen);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_refs(v, scope, secrets, vars, out, seen);
            }
        }
        _ => {}
    }
}

/// A literal value persisted under [`value_key`](pawrly_secrets::value_key), if any.
fn stored_literal(
    vars: Option<&dyn VariableValueStore>,
    var_id: &str,
) -> Result<Option<String>, ConfigError> {
    let Some(store) = vars else { return Ok(None) };
    Ok(store
        .get(&pawrly_secrets::value_key(var_id))
        .map_err(|e| ConfigError::Io(e.to_string()))?
        .map(|v| v.expose_secret().to_string()))
}

/// Resolve a single static variable to its concrete value (§4.3 precedence).
fn resolve_value(
    name: &str,
    def: &VariableDef,
    secrets: &dyn SecretStore,
    vars: Option<&dyn VariableValueStore>,
    scope_id: &str,
) -> Result<Value, ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };

    let input = def.input_key().unwrap_or(name);

    match def.kind {
        VarKind::Secret => {
            if let Some(lit) = stored_literal(vars, &format!("{scope_id}::{name}"))? {
                return Ok(Value::String(lit));
            }
            match secrets
                .get(input)
                .map_err(|e| ConfigError::Io(e.to_string()))?
            {
                Some(v) => Ok(Value::String(v.expose_secret().to_string())),
                None if !def.required => Ok(Value::Null),
                None => Err(err(format!(
                    "no value for secret input `{input}` (run `pawrly source connect`, \
                     set the env var, or add a secret-store entry)"
                ))),
            }
        }
        VarKind::Variable => {
            if let Some(lit) = stored_literal(vars, &format!("{scope_id}::{name}"))? {
                return coerce_str(name, def.var_type(), &def.choices, &lit);
            }
            if let Ok(raw) = std::env::var(input) {
                return coerce_str(name, def.var_type(), &def.choices, &raw);
            }
            if let Some(default) = &def.default {
                return Ok(default.clone());
            }
            if !def.required {
                return Ok(Value::Null);
            }
            Err(err(format!(
                "no value: env `{input}` is unset and no `default` is declared"
            )))
        }
    }
}

/// Coerce a raw string into the declared `type`.
fn coerce_str(name: &str, ty: VarType, choices: &[Value], raw: &str) -> Result<Value, ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };
    let t = raw.trim();
    match ty {
        VarType::String => Ok(Value::String(raw.to_string())),
        VarType::Integer => t
            .parse::<i64>()
            .map(|n| Value::Number(n.into()))
            .map_err(|_| err(format!("`{raw}` is not a valid integer"))),
        VarType::Number => {
            if let Ok(n) = t.parse::<i64>() {
                Ok(Value::Number(n.into()))
            } else {
                t.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| err(format!("`{raw}` is not a valid number")))
            }
        }
        VarType::Boolean => match t.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(err(format!("`{raw}` is not a valid boolean"))),
        },
        VarType::Enum => choices
            .iter()
            .find(|c| choice_matches(c, raw))
            .cloned()
            .ok_or_else(|| {
                let opts = choices
                    .iter()
                    .map(value_to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                err(format!("`{raw}` is not one of the allowed choices: {opts}"))
            }),
    }
}

fn choice_matches(choice: &Value, raw: &str) -> bool {
    match choice {
        Value::String(s) => s == raw,
        other => value_to_string(other) == raw,
    }
}

/// A discovery URL is https, or http to a loopback host (a local dev IdP).
fn discovery_url_ok(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Domain(h)) => h.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(a)) => a.is_loopback(),
            Some(url::Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// Build the engine-facing [`DynamicVarSpec`] for a dynamic variable.
fn build_dynamic_spec(
    name: &str,
    def: &VariableDef,
    secrets: &dyn SecretStore,
) -> Result<DynamicVarSpec, ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };
    let oauth = def
        .oauth_body()
        .ok_or_else(|| err("internal: dynamic variable without an `oauth` method".to_string()))?;

    let grant = oauth
        .get("grant")
        .and_then(|g| g.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let ep = |key: &str| -> Option<String> {
        oauth
            .get("endpoints")
            .and_then(|e| e.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let endpoints = Endpoints {
        discovery: ep("discovery"),
        authorization_url: ep("authorization_url"),
        device_authorization_url: ep("device_authorization_url"),
        token_url: ep("token_url"),
    };
    if let Some(d) = &endpoints.discovery
        && !discovery_url_ok(d)
    {
        return Err(err(
            "`endpoints.discovery` must be https (or http to a loopback host)".to_string(),
        ));
    }
    // With `endpoints.discovery`, missing endpoints are filled at runtime;
    // otherwise the grant's endpoints must be present here.
    let discovering = endpoints.discovery.is_some();
    let require = |present: bool, key: &str| -> Result<(), ConfigError> {
        if !discovering && !present {
            return Err(err(format!(
                "`{grant}` requires `endpoints.{key}` (or set `endpoints.discovery`)"
            )));
        }
        Ok(())
    };

    let client = oauth.get("client");
    let client_id = || -> Result<String, ConfigError> {
        resolve_client_field(name, client.and_then(|c| c.get("id")), false, secrets)?
            .ok_or_else(|| err(format!("`{grant}` requires `client.id`")))
    };
    let client_secret_opt =
        || resolve_client_field(name, client.and_then(|c| c.get("secret")), true, secrets);
    let transport = || match client
        .and_then(|c| c.get("secret"))
        .and_then(|s| s.get("transport"))
        .and_then(Value::as_str)
    {
        Some("basic_auth") => TokenTransport::BasicAuth,
        _ => TokenTransport::RequestBody,
    };

    match grant {
        "client_credentials" => {
            require(endpoints.token_url.is_some(), "token_url")?;
            Ok(DynamicVarSpec::ClientCredentials {
                endpoints,
                client_id: client_id()?,
                client_secret: client_secret_opt()?.ok_or_else(|| {
                    err("`client_credentials` requires `client.secret`".to_string())
                })?,
                scope: parse_scopes(oauth),
                audience: oauth
                    .get("audience")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                transport: transport(),
            })
        }
        "device_code" => {
            require(
                endpoints.device_authorization_url.is_some(),
                "device_authorization_url",
            )?;
            require(endpoints.token_url.is_some(), "token_url")?;
            Ok(DynamicVarSpec::DeviceCode {
                endpoints,
                client_id: client_id()?,
                client_secret: client_secret_opt()?,
                scope: parse_scopes(oauth),
            })
        }
        "authorization_code" => {
            require(endpoints.authorization_url.is_some(), "authorization_url")?;
            require(endpoints.token_url.is_some(), "token_url")?;
            let (redirect_uri, port_mode) = resolve_redirect(name, oauth)?;
            Ok(DynamicVarSpec::AuthorizationCode {
                endpoints,
                client_id: client_id()?,
                client_secret: client_secret_opt()?,
                redirect_uri,
                port_mode,
                pkce: !matches!(
                    oauth
                        .get("grant")
                        .and_then(|g| g.get("pkce"))
                        .and_then(Value::as_str),
                    Some("disabled")
                ),
                scope: parse_scopes(oauth),
                transport: transport(),
            })
        }
        "" => Err(err("`oauth` method requires `grant.type`".to_string())),
        other => Err(err(format!("unknown oauth `grant.type` `{other}`"))),
    }
}

/// Resolve an `authorization_code` `redirect:` block into `(redirect_uri, port_mode)`.
fn resolve_redirect(name: &str, oauth: &Value) -> Result<(String, PortMode), ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };
    let redirect = oauth
        .get("redirect")
        .ok_or_else(|| err("`authorization_code` requires `redirect`".to_string()))?;
    let uri = redirect
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| err("`redirect` requires `uri`".to_string()))?;
    let mut parsed =
        url::Url::parse(uri).map_err(|e| err(format!("invalid `redirect.uri`: {e}")))?;

    let explicit_port = match redirect.get("port") {
        None | Some(Value::Null) => None,
        Some(p) => Some(
            p.as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|p| *p != 0)
                .ok_or_else(
                    || err("`redirect.port` must be a port number (1–65535)".to_string()),
                )?,
        ),
    };
    let port = match (explicit_port, parsed.port()) {
        (Some(e), Some(u)) if e != u => {
            return Err(err(format!(
                "port set in both `redirect.uri` ({u}) and `redirect.port` ({e}) — specify it in one place"
            )));
        }
        (Some(e), _) => Some(e),
        (None, u) => u,
    };

    let port_mode = match redirect.get("port_mode").and_then(Value::as_str) {
        Some("fixed") => PortMode::Fixed,
        Some("random") => PortMode::Random,
        Some(other) => return Err(err(format!("unknown `redirect.port_mode` `{other}`"))),
        None if port.is_some() => PortMode::Fixed,
        None => PortMode::Random,
    };

    // `fixed` must embed the port in the URI (setup reads it back).
    if port_mode == PortMode::Fixed {
        let p = port.ok_or_else(|| {
            err("`redirect.port_mode: fixed` needs a `port` (set `redirect.port` or include one in `redirect.uri`)".to_string())
        })?;
        parsed
            .set_port(Some(p))
            .map_err(|()| err("`redirect.uri` does not accept a port".to_string()))?;
    }

    Ok((parsed.to_string(), port_mode))
}

/// Resolve an OAuth `client.id`/`client.secret` field; `input` is preferred over `default`.
fn resolve_client_field(
    name: &str,
    field: Option<&Value>,
    is_secret: bool,
    secrets: &dyn SecretStore,
) -> Result<Option<String>, ConfigError> {
    let err = |msg: String| ConfigError::Variable {
        name: name.to_string(),
        msg,
    };
    let Some(field) = field else {
        return Ok(None);
    };
    match field {
        Value::String(s) => Ok(Some(s.clone())),
        Value::Object(o) => {
            if let Some(input) = o.get("input").and_then(Value::as_str) {
                let resolved = if is_secret {
                    secrets
                        .get(input)
                        .map_err(|e| ConfigError::Io(e.to_string()))?
                        .map(|s| s.expose_secret().to_string())
                } else {
                    std::env::var(input).ok()
                };
                if let Some(v) = resolved {
                    return Ok(Some(v));
                }
            }
            if let Some(default) = o.get("default").and_then(Value::as_str) {
                return Ok(Some(default.to_string()));
            }
            Err(err(
                "OAuth `client` field has neither a resolvable `input` nor a `default`".to_string(),
            ))
        }
        _ => Err(err(
            "OAuth `client` field must be a string or a `{ input/default }` object".to_string(),
        )),
    }
}

fn parse_scopes(oauth: &Value) -> Option<String> {
    let scope = oauth.get("scopes")?.get("scope")?;
    let delim = match scope.get("delimiter").and_then(Value::as_str) {
        Some("comma") => ",",
        _ => " ",
    };
    let parts: Vec<&str> = scope
        .get("values")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    (!parts.is_empty()).then(|| parts.join(delim))
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse a string that is exactly `${var:NAME}`, returning `NAME`.
fn parse_ref(s: &str) -> Option<&str> {
    let body = s.trim().strip_prefix("${")?.strip_suffix('}')?;
    let (prefix, target) = body.split_once(':')?;
    (prefix == "var").then_some(target)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use pawrly_secrets::StaticStore;
    use serde_json::json;

    fn scope_from(v: Value) -> VariableScope {
        parse_block(&v, "test").unwrap()
    }

    #[test]
    fn parse_ref_only_exact_var() {
        assert_eq!(parse_ref("${var:API_BASE}"), Some("API_BASE"));
        assert_eq!(parse_ref("  ${var:X}  "), Some("X"));
        assert_eq!(parse_ref("${secret:X}"), None);
        assert_eq!(parse_ref("prefix-${var:X}"), None);
        assert_eq!(parse_ref("literal"), None);
    }

    #[test]
    fn variable_default_inlined() {
        let scope = scope_from(json!({
            "API_BASE": { "kind": "variable", "default": "https://api.example.com" }
        }));
        let mut src = json!({ "config": { "base_url": "${var:API_BASE}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["base_url"], json!("https://api.example.com"));
        assert!(binds.is_empty());
    }

    #[test]
    fn secret_resolves_from_store() {
        let secrets = StaticStore::new();
        secrets.insert("API_TOKEN", "shhh");
        let scope = scope_from(json!({ "API_TOKEN": { "kind": "secret" } }));
        let mut src = json!({ "config": { "token": "${var:API_TOKEN}" } });
        resolve_refs(&mut src, &scope, &secrets, None).unwrap();
        assert_eq!(src["config"]["token"], json!("shhh"));
    }

    #[test]
    fn input_overrides_name() {
        let secrets = StaticStore::new();
        secrets.insert("REAL_KEY", "v");
        let scope = scope_from(json!({
            "API_TOKEN": {
                "kind": "secret",
                "input": "REAL_KEY"
            }
        }));
        let mut src = json!({ "config": { "token": "${var:API_TOKEN}" } });
        resolve_refs(&mut src, &scope, &secrets, None).unwrap();
        assert_eq!(src["config"]["token"], json!("v"));
    }

    #[test]
    fn missing_static_refs_reports_unset_secret_only() {
        let secrets = StaticStore::new();
        let scope = scope_from(json!({
            "API_BASE": { "kind": "variable", "default": "https://api.example.com" },
            "API_TOKEN": { "kind": "secret" },
            "GH_TOKEN": {
                "kind": "secret",
                "oauth": { "grant": { "type": "client_credentials" } }
            }
        }));
        let src = json!({
            "config": {
                "base_url": "${var:API_BASE}",
                "token": "${var:API_TOKEN}",
                "extra": "${var:GH_TOKEN}"
            }
        });
        let refs = collect_static_refs(&src, &scope, &secrets, None);
        let missing: Vec<_> = refs.iter().filter(|r| !r.resolves).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "API_TOKEN");
        assert_eq!(missing[0].kind, VarKind::Secret);
        assert_eq!(missing[0].input_key, "API_TOKEN");
        assert_eq!(missing[0].var_id, "test::API_TOKEN");
    }

    #[test]
    fn missing_static_refs_dedups_and_ignores_undeclared() {
        let scope = scope_from(json!({ "T": { "kind": "secret" } }));
        let src = json!({
            "config": {
                "a": "${var:T}",
                "b": "${var:T}",
                "c": "${var:UNDECL}"
            }
        });
        let refs = collect_static_refs(&src, &scope, &StaticStore::new(), None);
        let missing: Vec<_> = refs.iter().filter(|r| !r.resolves).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "T");
    }

    #[derive(Debug, Default)]
    struct MapStore(std::sync::Mutex<std::collections::HashMap<String, String>>);

    impl VariableValueStore for MapStore {
        fn get(
            &self,
            id: &str,
        ) -> Result<Option<secrecy::SecretString>, pawrly_secrets::TokenStoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(id)
                .map(|v| secrecy::SecretString::from(v.clone())))
        }
        fn set(
            &self,
            id: &str,
            v: &secrecy::SecretString,
        ) -> Result<(), pawrly_secrets::TokenStoreError> {
            self.0
                .lock()
                .unwrap()
                .insert(id.to_string(), v.expose_secret().to_string());
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), pawrly_secrets::TokenStoreError> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
    }

    impl MapStore {
        fn put_literal(&self, var_id: &str, value: &str) {
            self.set(
                &pawrly_secrets::value_key(var_id),
                &secrecy::SecretString::from(value.to_string()),
            )
            .unwrap();
        }
    }

    #[test]
    fn stored_value_wins_over_input_chain() {
        let secrets = StaticStore::new();
        secrets.insert("REAL_KEY", "from-chain");
        let scope = scope_from(json!({
            "API_TOKEN": { "kind": "secret", "input": "REAL_KEY" }
        }));
        let store = MapStore::default();
        store.put_literal("test::API_TOKEN", "from-store");
        let mut src = json!({ "config": { "token": "${var:API_TOKEN}" } });
        resolve_refs(&mut src, &scope, &secrets, Some(&store)).unwrap();
        assert_eq!(src["config"]["token"], json!("from-store"));
    }

    #[test]
    fn collect_static_refs_marks_stored_secret_resolved() {
        let scope = scope_from(json!({ "T": { "kind": "secret" } }));
        let src = json!({ "config": { "x": "${var:T}" } });
        let refs = collect_static_refs(&src, &scope, &StaticStore::new(), None);
        assert_eq!(refs.len(), 1);
        assert!(!refs[0].resolves);
        let store = MapStore::default();
        store.put_literal("test::T", "v");
        let refs = collect_static_refs(&src, &scope, &StaticStore::new(), Some(&store));
        assert!(refs[0].resolves);
        assert_eq!(refs[0].var_id, "test::T");
    }

    #[test]
    fn stored_literal_overrides_oauth_and_skips_binding() {
        let scope = scope_from(json!({
            "GH_TOKEN": {
                "kind": "secret",
                "methods": [
                    { "type": "oauth", "grant": { "type": "device_code" },
                      "endpoints": { "device_authorization_url": "https://gh/d", "token_url": "https://gh/t" },
                      "client": { "id": { "default": "cid" } } },
                    { "type": "input", "input": "GH_PAT" }
                ]
            }
        }));
        let store = MapStore::default();
        store.put_literal("test::GH_TOKEN", "ghp_manual");
        let mut src = json!({ "config": { "token": "${var:GH_TOKEN}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), Some(&store)).unwrap();
        assert_eq!(
            src["config"]["token"],
            json!("ghp_manual"),
            "literal inlined"
        );
        assert!(
            binds.is_empty(),
            "no OAuth binding when a literal overrides it"
        );
    }

    #[test]
    fn without_literal_multimethod_emits_oauth_binding() {
        let scope = scope_from(json!({
            "GH_TOKEN": {
                "kind": "secret",
                "methods": [
                    { "type": "oauth", "grant": { "type": "device_code" },
                      "endpoints": { "device_authorization_url": "https://gh/d", "token_url": "https://gh/t" },
                      "client": { "id": { "default": "cid" } } },
                    { "type": "input", "input": "GH_PAT" }
                ]
            }
        }));
        let mut src = json!({ "config": { "token": "${var:GH_TOKEN}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["token"], json!("${var:GH_TOKEN}"));
        assert_eq!(binds.len(), 1);
        assert!(matches!(binds[0].spec, DynamicVarSpec::DeviceCode { .. }));
    }

    #[test]
    fn undeclared_reference_errors() {
        let scope = VariableScope::new();
        let mut src = json!({ "config": { "x": "${var:NOPE}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { name, .. } if name == "NOPE"));
    }

    #[test]
    fn secret_default_rejected() {
        let err =
            parse_block(&json!({ "X": { "kind": "secret", "default": "no" } }), "s").unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("default")));
    }

    #[test]
    fn reserved_prefix_rejected() {
        let err = parse_block(&json!({ "__pawrly_x": { "kind": "variable" } }), "s").unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("__pawrly")));
    }

    #[test]
    fn credential_like_name_must_be_secret() {
        for n in ["API_TOKEN", "db-password", "accessKey", "clientSecret"] {
            let err = parse_block(&json!({ n: { "kind": "variable" } }), "s").unwrap_err();
            assert!(
                matches!(&err, ConfigError::Variable { msg, .. } if msg.contains("credential")),
                "expected `{n}` to be rejected, got {err:?}"
            );
        }
        assert!(parse_block(&json!({ "SORT_KEY": { "kind": "variable" } }), "s").is_ok());
        assert!(parse_block(&json!({ "API_BASE": { "kind": "variable" } }), "s").is_ok());
    }

    #[test]
    fn variable_with_oauth_rejected() {
        let err = parse_block(
            &json!({
                "X": {
                    "kind": "variable",
                    "oauth": { "grant": { "type": "device_code" } }
                }
            }),
            "s",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Variable { .. }));
    }

    #[test]
    fn client_credentials_emits_binding_left_verbatim() {
        let scope = scope_from(json!({
            "API_TOKEN": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "client_credentials" },
                    "endpoints": { "token_url": "https://idp/token" },
                    "client": {
                        "id": { "default": "cid" },
                        "secret": { "default": "csecret", "transport": "basic_auth" }
                    },
                    "scopes": { "scope": { "delimiter": "space", "values": ["a", "b"] } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:API_TOKEN}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["token"], json!("${var:API_TOKEN}"));
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].name, "API_TOKEN");
        assert_eq!(binds[0].id, "test::API_TOKEN");
        match &binds[0].spec {
            DynamicVarSpec::ClientCredentials {
                endpoints,
                client_id,
                client_secret,
                scope,
                transport,
                ..
            } => {
                assert_eq!(endpoints.token_url.as_deref(), Some("https://idp/token"));
                assert_eq!(client_id, "cid");
                assert_eq!(client_secret, "csecret");
                assert_eq!(scope.as_deref(), Some("a b"));
                assert_eq!(*transport, TokenTransport::BasicAuth);
            }
            other => panic!("expected ClientCredentials, got {other:?}"),
        }
    }

    #[test]
    fn client_secret_input_resolves_from_store() {
        let secrets = StaticStore::new();
        secrets.insert("CS", "from-store");
        let scope = scope_from(json!({
            "T": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "client_credentials" },
                    "endpoints": { "token_url": "https://idp/token" },
                    "client": { "id": "cid", "secret": { "input": "CS" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:T}" } });
        let binds = resolve_refs(&mut src, &scope, &secrets, None).unwrap();
        match &binds[0].spec {
            DynamicVarSpec::ClientCredentials { client_secret, .. } => {
                assert_eq!(client_secret, "from-store");
            }
            other => panic!("expected ClientCredentials, got {other:?}"),
        }
    }

    #[test]
    fn device_code_emits_binding() {
        let scope = scope_from(json!({
            "GH_TOKEN": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "device_code" },
                    "endpoints": {
                        "device_authorization_url": "https://gh/device/code",
                        "token_url": "https://gh/token"
                    },
                    "client": { "id": { "default": "cid" } },
                    "scopes": { "scope": { "delimiter": "space", "values": ["repo", "read:org"] } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:GH_TOKEN}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["token"], json!("${var:GH_TOKEN}"));
        assert_eq!(binds.len(), 1);
        match &binds[0].spec {
            DynamicVarSpec::DeviceCode {
                endpoints,
                client_id,
                client_secret,
                scope,
                ..
            } => {
                assert_eq!(
                    endpoints.device_authorization_url.as_deref(),
                    Some("https://gh/device/code")
                );
                assert_eq!(client_id, "cid");
                assert!(client_secret.is_none());
                assert_eq!(scope.as_deref(), Some("repo read:org"));
            }
            other => panic!("expected DeviceCode, got {other:?}"),
        }
    }

    fn auth_code_spec(redirect: Value) -> DynamicVarSpec {
        let scope = scope_from(json!({
            "SF_TOKEN": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "authorization_code" },
                    "redirect": redirect,
                    "endpoints": {
                        "authorization_url": "https://idp/authorize",
                        "token_url": "https://idp/token"
                    },
                    "client": { "id": { "default": "cid" }, "secret": { "default": "csec" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:SF_TOKEN}" } });
        let mut binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        binds.remove(0).spec
    }

    #[test]
    fn authorization_code_emits_binding_with_pkce_default_on() {
        match auth_code_spec(json!({ "uri": "http://127.0.0.1/callback", "port": 5000 })) {
            DynamicVarSpec::AuthorizationCode {
                endpoints,
                redirect_uri,
                port_mode,
                pkce,
                client_secret,
                ..
            } => {
                assert_eq!(
                    endpoints.authorization_url.as_deref(),
                    Some("https://idp/authorize")
                );
                assert_eq!(redirect_uri, "http://127.0.0.1:5000/callback");
                assert_eq!(port_mode, pawrly_core::PortMode::Fixed);
                assert!(pkce, "PKCE defaults on");
                assert_eq!(client_secret.as_deref(), Some("csec"));
            }
            other => panic!("expected AuthorizationCode, got {other:?}"),
        }
    }

    #[test]
    fn authorization_code_redirect_port_derivation() {
        match auth_code_spec(json!({ "uri": "http://127.0.0.1/callback" })) {
            DynamicVarSpec::AuthorizationCode {
                redirect_uri,
                port_mode,
                ..
            } => {
                assert_eq!(redirect_uri, "http://127.0.0.1/callback");
                assert_eq!(port_mode, pawrly_core::PortMode::Random);
            }
            other => panic!("expected AuthorizationCode, got {other:?}"),
        }
        match auth_code_spec(json!({ "uri": "http://127.0.0.1:7777/callback" })) {
            DynamicVarSpec::AuthorizationCode {
                redirect_uri,
                port_mode,
                ..
            } => {
                assert_eq!(redirect_uri, "http://127.0.0.1:7777/callback");
                assert_eq!(port_mode, pawrly_core::PortMode::Fixed);
            }
            other => panic!("expected AuthorizationCode, got {other:?}"),
        }
    }

    #[test]
    fn authorization_code_port_in_uri_and_field() {
        match auth_code_spec(json!({ "uri": "http://127.0.0.1:5000/callback", "port": 5000 })) {
            DynamicVarSpec::AuthorizationCode {
                redirect_uri,
                port_mode,
                ..
            } => {
                assert_eq!(redirect_uri, "http://127.0.0.1:5000/callback");
                assert_eq!(port_mode, pawrly_core::PortMode::Fixed);
            }
            other => panic!("expected AuthorizationCode, got {other:?}"),
        }
        let scope = scope_from(json!({
            "SF_TOKEN": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "authorization_code" },
                    "redirect": { "uri": "http://127.0.0.1:8080/callback", "port": 5000 },
                    "endpoints": {
                        "authorization_url": "https://idp/authorize",
                        "token_url": "https://idp/token"
                    },
                    "client": { "id": { "default": "cid" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:SF_TOKEN}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("in one place")));
    }

    #[test]
    fn authorization_code_fixed_without_port_errors() {
        let scope = scope_from(json!({
            "SF_TOKEN": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "authorization_code" },
                    "redirect": { "uri": "http://127.0.0.1/callback", "port_mode": "fixed" },
                    "endpoints": {
                        "authorization_url": "https://idp/authorize",
                        "token_url": "https://idp/token"
                    },
                    "client": { "id": { "default": "cid" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:SF_TOKEN}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("needs a `port`")));
    }

    #[test]
    fn discovery_endpoint_accepted_without_explicit_urls() {
        let scope = scope_from(json!({
            "T": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "device_code" },
                    "endpoints": { "discovery": "https://idp/.well-known/openid-configuration" },
                    "client": { "id": { "default": "cid" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:T}" } });
        let binds = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        match &binds[0].spec {
            DynamicVarSpec::DeviceCode { endpoints, .. } => {
                assert_eq!(
                    endpoints.discovery.as_deref(),
                    Some("https://idp/.well-known/openid-configuration")
                );
                assert!(endpoints.device_authorization_url.is_none());
                assert!(endpoints.token_url.is_none());
            }
            other => panic!("expected DeviceCode, got {other:?}"),
        }
    }

    #[test]
    fn discovery_must_be_https_or_loopback() {
        let scope = scope_from(json!({
            "T": {
                "kind": "secret",
                "oauth": {
                    "grant": { "type": "client_credentials" },
                    "endpoints": { "discovery": "http://idp.example.com/.well-known/x" },
                    "client": { "id": { "default": "c" }, "secret": { "default": "s" } }
                }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:T}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("https")));
    }

    #[test]
    fn malformed_device_code_missing_endpoints_errors() {
        let scope = scope_from(json!({
            "T": {
                "kind": "secret",
                "oauth": { "grant": { "type": "device_code" } }
            }
        }));
        let mut src = json!({ "config": { "token": "${var:T}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(
            matches!(err, ConfigError::Variable { msg, .. } if msg.contains("device_authorization_url"))
        );
    }

    #[test]
    fn skips_declaration_block() {
        let scope = scope_from(json!({ "A": { "kind": "variable", "default": "x" } }));
        let mut src = json!({
            "variables": { "A": { "kind": "variable", "default": "${var:A}" } },
            "config": { "v": "${var:A}" }
        });
        resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["v"], json!("x"));
        assert_eq!(src["variables"]["A"]["default"], json!("${var:A}"));
    }

    #[test]
    fn merge_inner_shadows_outer() {
        let mut base = scope_from(json!({ "T": { "kind": "variable", "default": "outer" } }));
        let inner = parse_block(
            &json!({ "T": { "kind": "variable", "default": "inner" } }),
            "inner",
        )
        .unwrap();
        merge_into(&mut base, &inner);
        assert_eq!(base["T"].def.default, Some(json!("inner")));
        assert_eq!(base["T"].scope_id, "inner");
    }

    #[test]
    fn coerce_str_parses_scalar_types() {
        assert_eq!(
            coerce_str("N", VarType::Integer, &[], "42").unwrap(),
            json!(42)
        );
        assert_eq!(
            coerce_str("N", VarType::Number, &[], "42").unwrap(),
            json!(42)
        );
        assert_eq!(
            coerce_str("N", VarType::Number, &[], "1.5").unwrap(),
            json!(1.5)
        );
        assert_eq!(
            coerce_str("B", VarType::Boolean, &[], "true").unwrap(),
            json!(true)
        );
        assert_eq!(
            coerce_str("B", VarType::Boolean, &[], "no").unwrap(),
            json!(false)
        );
        assert_eq!(
            coerce_str("S", VarType::String, &[], " x ").unwrap(),
            json!(" x ")
        );
        assert!(coerce_str("N", VarType::Integer, &[], "nope").is_err());
        assert!(coerce_str("N", VarType::Integer, &[], "1.5").is_err());
        assert!(coerce_str("B", VarType::Boolean, &[], "maybe").is_err());
    }

    #[test]
    fn coerce_str_enum_validates_membership() {
        let choices = vec![json!("us"), json!("eu")];
        assert_eq!(
            coerce_str("R", VarType::Enum, &choices, "eu").unwrap(),
            json!("eu")
        );
        assert!(coerce_str("R", VarType::Enum, &choices, "ap").is_err());
    }

    #[test]
    fn typed_default_inlined_with_json_type() {
        let scope = scope_from(json!({
            "PAGE_SIZE": { "kind": "variable", "type": "number", "default": 100 },
            "VERIFY": { "kind": "variable", "type": "boolean", "default": true },
            "REGION": { "kind": "variable", "type": "enum", "choices": ["us", "eu"], "default": "us" }
        }));
        let mut src = json!({ "config": {
            "page_size": "${var:PAGE_SIZE}",
            "verify": "${var:VERIFY}",
            "region": "${var:REGION}"
        } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["page_size"], json!(100));
        assert_eq!(src["config"]["verify"], json!(true));
        assert_eq!(src["config"]["region"], json!("us"));
    }

    #[test]
    fn enum_requires_non_empty_choices() {
        let err =
            parse_block(&json!({ "R": { "kind": "variable", "type": "enum" } }), "s").unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("choices")));
    }

    #[test]
    fn enum_default_must_be_a_choice() {
        let err = parse_block(
            &json!({ "R": { "kind": "variable", "type": "enum", "choices": ["us", "eu"], "default": "ap" } }),
            "s",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("choices")));
        assert!(
            parse_block(
                &json!({ "R": { "kind": "variable", "type": "enum", "choices": ["us", "eu"], "default": "us" } }),
                "s",
            )
            .is_ok()
        );
    }

    #[test]
    fn choices_without_enum_rejected() {
        let err = parse_block(
            &json!({ "X": { "kind": "variable", "type": "string", "choices": ["a"] } }),
            "s",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("choices")));
    }

    #[test]
    fn default_type_mismatch_rejected() {
        let err = parse_block(
            &json!({ "N": { "kind": "variable", "type": "number", "default": "not-a-number" } }),
            "s",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Variable { msg, .. } if msg.contains("type: number")));
    }

    #[test]
    fn type_and_choices_rejected_on_secret() {
        let bad_type =
            parse_block(&json!({ "T": { "kind": "secret", "type": "number" } }), "s").unwrap_err();
        assert!(matches!(bad_type, ConfigError::Variable { msg, .. } if msg.contains("type")));
        let bad_choices =
            parse_block(&json!({ "T": { "kind": "secret", "choices": ["a"] } }), "s").unwrap_err();
        assert!(
            matches!(bad_choices, ConfigError::Variable { msg, .. } if msg.contains("choices"))
        );
    }

    #[test]
    fn variables_are_required_by_default() {
        let scope =
            scope_from(json!({ "HOST": { "kind": "variable", "input": "PAWRLY_UNSET_REQ_A1" } }));
        let mut src = json!({ "config": { "x": "${var:HOST}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { name, .. } if name == "HOST"));
        assert!(scope["HOST"].def.required, "required defaults to true");
    }

    #[test]
    fn optional_variable_resolves_to_null_when_unset() {
        let scope = scope_from(json!({
            "OPT": { "kind": "variable", "required": false, "input": "PAWRLY_UNSET_OPT_A2" }
        }));
        let mut src = json!({ "config": { "x": "${var:OPT}" } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["x"], Value::Null);
    }

    #[test]
    fn optional_secret_resolves_to_null_when_unset() {
        let scope = scope_from(json!({ "TOKEN": { "kind": "secret", "required": false } }));
        let mut src = json!({ "config": { "token": "${var:TOKEN}" } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["token"], Value::Null);
    }

    #[test]
    fn optional_variable_still_prefers_default() {
        let scope = scope_from(json!({
            "OPT": { "kind": "variable", "required": false, "default": "fallback" }
        }));
        let mut src = json!({ "config": { "v": "${var:OPT}" } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), None).unwrap();
        assert_eq!(src["config"]["v"], json!("fallback"));
    }

    #[test]
    fn stored_value_overrides_default_for_variable() {
        let scope = scope_from(json!({
            "REGION": { "kind": "variable", "type": "enum", "choices": ["us", "eu"], "default": "us" }
        }));
        let store = MapStore::default();
        store.put_literal("test::REGION", "eu");
        let mut src = json!({ "config": { "r": "${var:REGION}" } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), Some(&store)).unwrap();
        assert_eq!(src["config"]["r"], json!("eu"));
    }

    #[test]
    fn stored_value_coerced_to_declared_type() {
        let scope = scope_from(json!({ "PAGE": { "kind": "variable", "type": "number" } }));
        let store = MapStore::default();
        store.put_literal("test::PAGE", "100");
        let mut src = json!({ "config": { "n": "${var:PAGE}" } });
        resolve_refs(&mut src, &scope, &StaticStore::new(), Some(&store)).unwrap();
        assert_eq!(src["config"]["n"], json!(100));
    }

    #[test]
    fn stored_enum_value_still_validated() {
        let scope = scope_from(json!({
            "REGION": { "kind": "variable", "type": "enum", "choices": ["us", "eu"] }
        }));
        let store = MapStore::default();
        store.put_literal("test::REGION", "ap");
        let mut src = json!({ "config": { "r": "${var:REGION}" } });
        let err = resolve_refs(&mut src, &scope, &StaticStore::new(), Some(&store)).unwrap_err();
        assert!(matches!(err, ConfigError::Variable { .. }));
    }

    #[test]
    fn coerce_validates_input_for_both_kinds() {
        let int: VariableDef =
            serde_json::from_value(json!({ "kind": "variable", "type": "integer" })).unwrap();
        assert_eq!(int.coerce("42").unwrap(), json!(42));
        assert!(int.coerce("nope").is_err());
        let sec: VariableDef = serde_json::from_value(json!({ "kind": "secret" })).unwrap();
        assert_eq!(sec.coerce("anything").unwrap(), json!("anything"));
    }

    #[test]
    fn has_input_method_detects_paste_path() {
        let multi = scope_from(json!({
            "T": { "kind": "secret", "methods": [
                { "type": "oauth", "grant": { "type": "device_code" },
                  "endpoints": { "device_authorization_url": "https://d", "token_url": "https://t" },
                  "client": { "id": { "default": "c" } } },
                { "type": "input", "input": "T_PAT" }
            ] }
        }));
        assert!(multi["T"].def.is_dynamic());
        assert!(multi["T"].def.has_input_method());

        let oauth_only = scope_from(json!({
            "T": { "kind": "secret", "oauth": { "grant": { "type": "device_code" },
                "endpoints": { "device_authorization_url": "https://d", "token_url": "https://t" },
                "client": { "id": { "default": "c" } } } }
        }));
        assert!(oauth_only["T"].def.is_dynamic());
        assert!(!oauth_only["T"].def.has_input_method());

        let bare = scope_from(json!({ "T": { "kind": "secret" } }));
        assert!(!bare["T"].def.is_dynamic());
        assert!(!bare["T"].def.has_input_method());
    }
}
