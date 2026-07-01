//! Error types shared across the workspace.
//!
//! Each crate may define its own internal `Error`, but everything that
//! crosses a crate boundary collapses into one of these.

use std::time::Duration;

use thiserror::Error;

/// Stable machine-readable error code, surfaced over MCP and gRPC.
pub type ErrorCode = &'static str;

/// Top-level Pawrly error. Every public API returns this (or a `Result`
/// that wraps it).
#[derive(Debug, Error)]
pub enum PawrlyError {
    #[error("{0}")]
    Engine(#[from] EngineError),

    #[error("{0}")]
    Source(#[from] SourceError),

    #[error("{0}")]
    Safety(#[from] SafetyError),

    #[error("{0}")]
    Config(#[from] ConfigError),
}

impl PawrlyError {
    /// Stable error code suitable for surfacing over MCP / gRPC metadata.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Engine(e) => e.code(),
            Self::Source(e) => e.code(),
            Self::Safety(e) => e.code(),
            Self::Config(e) => e.code(),
        }
    }
}

/// Errors raised by the engine layer.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("unknown source kind: {0}")]
    UnknownKind(String),

    #[error("source `{name}` ({kind}) failed to register: {source}")]
    SourceRegistration {
        name: String,
        kind: String,
        #[source]
        source: SourceError,
    },

    #[error("unknown table `{0}`")]
    UnknownTable(String),

    #[error("unknown function `{0}`")]
    UnknownFunction(String),

    #[error("safety check failed: {0}")]
    Safety(#[from] SafetyError),

    #[error("query timed out after {0:?}")]
    Timeout(Duration),

    #[error("query exceeded memory budget of {0} bytes")]
    OutOfMemory(u64),

    #[error("invalid SQL: {0}")]
    InvalidSql(String),

    #[error("semantic plan error: {0}")]
    SemanticPlan(String),

    #[error("query was cancelled")]
    Cancelled,

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl EngineError {
    /// Stable error code suitable for clients (MCP, gRPC).
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::UnknownKind(_) => "PAWRLY_UNKNOWN_KIND",
            Self::SourceRegistration { .. } => "PAWRLY_SOURCE_REGISTRATION",
            Self::UnknownTable(_) => "PAWRLY_UNKNOWN_TABLE",
            Self::UnknownFunction(_) => "PAWRLY_UNKNOWN_FUNCTION",
            Self::Safety(s) => s.code(),
            Self::Timeout(_) => "PAWRLY_TIMEOUT",
            Self::OutOfMemory(_) => "PAWRLY_OOM",
            Self::InvalidSql(_) => "PAWRLY_INVALID_SQL",
            Self::SemanticPlan(_) => "PAWRLY_SEMANTIC_PLAN",
            Self::Cancelled => "PAWRLY_CANCELLED",
            Self::Protocol(_) => "PAWRLY_PROTOCOL",
            Self::Internal(_) => "PAWRLY_INTERNAL",
        }
    }
}

/// Errors that come from a source while fetching or describing data.
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("source `{0}` is unreachable")]
    Unreachable(String),

    #[error("source `{0}` returned a schema that does not match its declaration: {1}")]
    Schema(String, String),

    #[error("source `{0}` is unauthorized; check credentials")]
    Unauthorized(String),

    #[error("source `{0}` denied the request: {1}")]
    Forbidden(String, String),

    #[error("source `{0}` is rate-limited; retry-after {retry_after:?}", retry_after = .1)]
    RateLimited(String, Option<Duration>),

    #[error("source `{0}` is unavailable in this build: {1}")]
    Unavailable(String, String),

    #[error("source `{0}`: {1}")]
    Other(String, String),
}

impl SourceError {
    /// Stable error code suitable for clients.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Unreachable(_) => "PAWRLY_SOURCE_UNREACHABLE",
            Self::Schema(_, _) => "PAWRLY_SOURCE_SCHEMA_MISMATCH",
            Self::Unauthorized(_) => "PAWRLY_SOURCE_UNAUTHORIZED",
            Self::Forbidden(_, _) => "PAWRLY_SOURCE_FORBIDDEN",
            Self::RateLimited(_, _) => "PAWRLY_SOURCE_RATE_LIMITED",
            Self::Unavailable(_, _) => "PAWRLY_SOURCE_UNAVAILABLE",
            Self::Other(_, _) => "PAWRLY_SOURCE_OTHER",
        }
    }
}

/// Errors raised by the safety pre-check before scan execution.
#[derive(Debug, Error)]
pub enum SafetyError {
    #[error("refusing to scan `{table}` without a filter on `{column}`")]
    MissingRequiredFilter { table: String, column: String },

    #[error("refusing to scan `{table}` without any filter")]
    NoFilters { table: String },

    #[error("refusing to scan `{table}`: would return more than {max_rows} rows")]
    TooManyRows { table: String, max_rows: u64 },

    #[error("refusing to scan `{table}`: would page more than {max_pages} times")]
    TooManyPages { table: String, max_pages: u32 },

    /// A required predicate references `${param:NAME}` but the param was not
    /// supplied on the request.
    #[error("refusing to scan `{table}`: required predicate references unbound param `{name}`")]
    UnboundParam { table: String, name: String },

    /// A required predicate could not be applied (e.g. it references a column
    /// the rollup-substituted scan does not expose).
    #[error(
        "refusing to scan `{table}`: required predicate `{predicate}` could not be applied: {reason}"
    )]
    PredicateUnsatisfied {
        table: String,
        predicate: String,
        reason: String,
    },
}

impl SafetyError {
    /// Stable error code suitable for clients.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::MissingRequiredFilter { .. } => "PAWRLY_SAFETY_REQUIRED_FILTER",
            Self::NoFilters { .. } => "PAWRLY_SAFETY_NO_FILTERS",
            Self::TooManyRows { .. } => "PAWRLY_SAFETY_TOO_MANY_ROWS",
            Self::TooManyPages { .. } => "PAWRLY_SAFETY_TOO_MANY_PAGES",
            Self::UnboundParam { .. } => "PAWRLY_SAFETY_UNBOUND_PARAM",
            Self::PredicateUnsatisfied { .. } => "PAWRLY_SAFETY_PREDICATE_UNSATISFIED",
        }
    }
}

/// Errors raised by config loading and validation.
///
/// Multiple `ConfigError`s may be accumulated and reported together so
/// users see all problems at once.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("at {path}: {msg}")]
    Schema { path: String, msg: String },

    #[error("source `{source_name}` table `{table}`: {msg}")]
    Table {
        source_name: String,
        table: String,
        msg: String,
    },

    #[error("source `{0}`: {1}")]
    Source(String, String),

    #[error("semantic model `{model}`: {msg}")]
    SemanticInvalid { model: String, msg: String },

    #[error("function `{namespace}.{name}`: {msg}")]
    FunctionInvalid {
        namespace: String,
        name: String,
        msg: String,
    },

    #[error("unresolved secret reference: {0}")]
    UnresolvedSecret(String),

    #[error("unresolved env reference: {0}")]
    UnresolvedEnv(String),

    #[error("variable `{name}`: {msg}")]
    Variable { name: String, msg: String },

    #[error("could not read referenced file `{path}`: {msg}")]
    ReadFile { path: String, msg: String },

    #[error("include/from cycle: {0}")]
    IncludeCycle(String),

    #[error("invalid duration `{0}`")]
    Duration(String),

    #[error("unknown source kind `{0}`")]
    UnknownKind(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("yaml parse error: {0}")]
    Yaml(String),

    #[error("config version `{0}` is not supported (only `1` is)")]
    UnsupportedVersion(u32),

    #[error("secrets file `{path}` must be mode 0600 (got {mode:o})")]
    InsecureSecretsFile { path: String, mode: u32 },
}

impl ConfigError {
    /// Stable error code suitable for clients.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Schema { .. } => "PAWRLY_CONFIG_SCHEMA",
            Self::Table { .. } => "PAWRLY_CONFIG_TABLE",
            Self::Source(_, _) => "PAWRLY_CONFIG_SOURCE",
            Self::SemanticInvalid { .. } => "PAWRLY_CONFIG_SEMANTIC_INVALID",
            Self::FunctionInvalid { .. } => "PAWRLY_CONFIG_FUNCTION_INVALID",
            Self::UnresolvedSecret(_) => "PAWRLY_CONFIG_UNRESOLVED_SECRET",
            Self::UnresolvedEnv(_) => "PAWRLY_CONFIG_UNRESOLVED_ENV",
            Self::Variable { .. } => "PAWRLY_CONFIG_VARIABLE",
            Self::ReadFile { .. } => "PAWRLY_CONFIG_READ_FILE",
            Self::IncludeCycle(_) => "PAWRLY_CONFIG_INCLUDE_CYCLE",
            Self::Duration(_) => "PAWRLY_CONFIG_DURATION",
            Self::UnknownKind(_) => "PAWRLY_CONFIG_UNKNOWN_KIND",
            Self::Io(_) => "PAWRLY_CONFIG_IO",
            Self::Yaml(_) => "PAWRLY_CONFIG_YAML",
            Self::UnsupportedVersion(_) => "PAWRLY_CONFIG_UNSUPPORTED_VERSION",
            Self::InsecureSecretsFile { .. } => "PAWRLY_CONFIG_INSECURE_SECRETS_FILE",
        }
    }
}

/// A bundle of one or more config errors. Returned by validation so users
/// see every problem at once instead of fixing them one round-trip at a time.
#[derive(Debug, Default)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl ConfigErrors {
    /// True iff there are no errors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of accumulated errors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Push another error onto the accumulator.
    pub fn push(&mut self, err: ConfigError) {
        self.0.push(err);
    }
}

impl std::fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, err) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        let e = EngineError::Cancelled;
        assert_eq!(e.code(), "PAWRLY_CANCELLED");

        let s = SafetyError::MissingRequiredFilter {
            table: "warehouse.x".into(),
            column: "k".into(),
        };
        assert_eq!(s.code(), "PAWRLY_SAFETY_REQUIRED_FILTER");

        let p: PawrlyError = e.into();
        assert_eq!(p.code(), "PAWRLY_CANCELLED");
    }

    #[test]
    fn config_errors_aggregates() {
        let mut errs = ConfigErrors::default();
        assert!(errs.is_empty());
        errs.push(ConfigError::UnsupportedVersion(2));
        errs.push(ConfigError::UnknownKind("flubber".into()));
        assert_eq!(errs.len(), 2);
        let s = format!("{errs}");
        assert!(s.contains("not supported"));
        assert!(s.contains("flubber"));
    }
}
