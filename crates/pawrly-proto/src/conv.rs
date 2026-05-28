//! Conversion between `pawrly_core` runtime types and the wire types in
//! [`crate::v1`]. Used by both `pawrly-server` (encoding outgoing responses)
//! and `pawrly-client` (decoding them).
//!
//! Conversions are deliberately fallible only where the wire form admits
//! ambiguity — e.g., the proto `SourceKind` enum has an `UNSPECIFIED` member
//! that doesn't appear in the Rust enum.

use chrono::{DateTime, TimeZone, Utc};
use prost_types::Timestamp;

use pawrly_core as core;

use crate::v1;

// ---- SourceKind ----

impl From<core::SourceKind> for v1::SourceKind {
    fn from(k: core::SourceKind) -> Self {
        match k {
            core::SourceKind::Http => Self::Http,
            core::SourceKind::Github => Self::Github,
            core::SourceKind::Linear => Self::Linear,
            core::SourceKind::Stripe => Self::Stripe,
            core::SourceKind::Sentry => Self::Sentry,
            core::SourceKind::Datadog => Self::Datadog,
            core::SourceKind::Slack => Self::Slack,
            core::SourceKind::Notion => Self::Notion,
            core::SourceKind::Ai => Self::Ai,
            core::SourceKind::File => Self::File,
            core::SourceKind::Postgres => Self::Postgres,
            core::SourceKind::Mysql => Self::Mysql,
            core::SourceKind::Sqlite => Self::Sqlite,
            core::SourceKind::Snowflake => Self::Snowflake,
            core::SourceKind::Bigquery => Self::Bigquery,
            core::SourceKind::Redshift => Self::Redshift,
            core::SourceKind::Iceberg => Self::Iceberg,
            core::SourceKind::Delta => Self::Delta,
            core::SourceKind::S3 => Self::S3,
            core::SourceKind::Gcs => Self::Gcs,
            core::SourceKind::Azure => Self::Azure,
        }
    }
}

impl TryFrom<v1::SourceKind> for core::SourceKind {
    type Error = ConvError;

    fn try_from(k: v1::SourceKind) -> Result<Self, ConvError> {
        match k {
            v1::SourceKind::Unspecified => Err(ConvError::UnspecifiedSourceKind),
            v1::SourceKind::Http => Ok(Self::Http),
            v1::SourceKind::Github => Ok(Self::Github),
            v1::SourceKind::Linear => Ok(Self::Linear),
            v1::SourceKind::Stripe => Ok(Self::Stripe),
            v1::SourceKind::Sentry => Ok(Self::Sentry),
            v1::SourceKind::Datadog => Ok(Self::Datadog),
            v1::SourceKind::Slack => Ok(Self::Slack),
            v1::SourceKind::Notion => Ok(Self::Notion),
            v1::SourceKind::Ai => Ok(Self::Ai),
            v1::SourceKind::File => Ok(Self::File),
            v1::SourceKind::Postgres => Ok(Self::Postgres),
            v1::SourceKind::Mysql => Ok(Self::Mysql),
            v1::SourceKind::Sqlite => Ok(Self::Sqlite),
            v1::SourceKind::Snowflake => Ok(Self::Snowflake),
            v1::SourceKind::Bigquery => Ok(Self::Bigquery),
            v1::SourceKind::Redshift => Ok(Self::Redshift),
            v1::SourceKind::Iceberg => Ok(Self::Iceberg),
            v1::SourceKind::Delta => Ok(Self::Delta),
            v1::SourceKind::S3 => Ok(Self::S3),
            v1::SourceKind::Gcs => Ok(Self::Gcs),
            v1::SourceKind::Azure => Ok(Self::Azure),
        }
    }
}

impl From<core::CacheMode> for v1::CacheMode {
    fn from(m: core::CacheMode) -> Self {
        match m {
            core::CacheMode::None => Self::None,
            core::CacheMode::Ttl => Self::Ttl,
            core::CacheMode::Refresh => Self::Refresh,
            core::CacheMode::Cron => Self::Cron,
            core::CacheMode::Append => Self::Append,
        }
    }
}

// ---- TableName ----

impl From<&core::TableName> for v1::TableName {
    fn from(n: &core::TableName) -> Self {
        Self {
            schema: n.schema.clone(),
            table: n.table.clone(),
        }
    }
}

impl From<core::TableName> for v1::TableName {
    fn from(n: core::TableName) -> Self {
        Self {
            schema: n.schema,
            table: n.table,
        }
    }
}

impl From<v1::TableName> for core::TableName {
    fn from(n: v1::TableName) -> Self {
        Self {
            schema: n.schema,
            table: n.table,
        }
    }
}

// ---- ColumnSpec ----

impl From<core::ColumnSpec> for v1::ColumnSpec {
    fn from(c: core::ColumnSpec) -> Self {
        Self {
            name: c.name,
            data_type: c.data_type,
            nullable: c.nullable,
            description: c.description.unwrap_or_default(),
            is_filter_pushable: c.is_filter_pushable,
            is_required_filter: c.is_required_filter,
        }
    }
}

impl From<v1::ColumnSpec> for core::ColumnSpec {
    fn from(c: v1::ColumnSpec) -> Self {
        Self {
            name: c.name,
            data_type: c.data_type,
            nullable: c.nullable,
            description: if c.description.is_empty() {
                None
            } else {
                Some(c.description)
            },
            is_filter_pushable: c.is_filter_pushable,
            is_required_filter: c.is_required_filter,
        }
    }
}

// ---- TableInfo ----

impl From<core::TableInfo> for v1::TableInfo {
    fn from(t: core::TableInfo) -> Self {
        Self {
            name: Some(t.name.into()),
            kind: v1::SourceKind::from(t.kind) as i32,
            description: t.description.unwrap_or_default(),
            row_count_estimate: t.row_count_estimate,
            cached: t.cached,
            required_filters: t.required_filters,
        }
    }
}

impl TryFrom<v1::TableInfo> for core::TableInfo {
    type Error = ConvError;

    fn try_from(t: v1::TableInfo) -> Result<Self, ConvError> {
        let name = t.name.ok_or(ConvError::Missing("TableInfo.name"))?;
        let kind = v1::SourceKind::try_from(t.kind)
            .map_err(|_| ConvError::Invalid("TableInfo.kind"))?
            .try_into()?;
        Ok(Self {
            name: name.into(),
            kind,
            description: if t.description.is_empty() {
                None
            } else {
                Some(t.description)
            },
            row_count_estimate: t.row_count_estimate,
            cached: t.cached,
            required_filters: t.required_filters,
        })
    }
}

// ---- SourceInfo ----

impl From<core::SourceInfo> for v1::SourceInfo {
    fn from(s: core::SourceInfo) -> Self {
        Self {
            name: s.name,
            kind: v1::SourceKind::from(s.kind) as i32,
            status: match s.status {
                core::SourceStatus::Ok => v1::SourceStatus::Ok as i32,
                core::SourceStatus::Unavailable => v1::SourceStatus::Unavailable as i32,
            },
            status_detail: s.status_detail.unwrap_or_default(),
            table_count: s.table_count,
            registered_at: Some(timestamp_from(s.registered_at)),
        }
    }
}

impl TryFrom<v1::SourceInfo> for core::SourceInfo {
    type Error = ConvError;

    fn try_from(s: v1::SourceInfo) -> Result<Self, ConvError> {
        let kind = v1::SourceKind::try_from(s.kind)
            .map_err(|_| ConvError::Invalid("SourceInfo.kind"))?
            .try_into()?;
        let status = match v1::SourceStatus::try_from(s.status) {
            Ok(v1::SourceStatus::Ok) => core::SourceStatus::Ok,
            Ok(v1::SourceStatus::Unavailable) => core::SourceStatus::Unavailable,
            _ => return Err(ConvError::Invalid("SourceInfo.status")),
        };
        Ok(Self {
            name: s.name,
            kind,
            status,
            status_detail: if s.status_detail.is_empty() {
                None
            } else {
                Some(s.status_detail)
            },
            table_count: s.table_count,
            registered_at: s
                .registered_at
                .map(timestamp_to)
                .unwrap_or_else(|| Utc::now()),
        })
    }
}

// ---- HealthReport ----

impl From<core::HealthReport> for v1::HealthResponse {
    fn from(h: core::HealthReport) -> Self {
        Self {
            ok: h.ok,
            version: h.version,
            active_queries: h.active_queries,
            sources_ok: h.sources_ok,
            sources_unavailable: h.sources_unavailable,
        }
    }
}

impl From<v1::HealthResponse> for core::HealthReport {
    fn from(h: v1::HealthResponse) -> Self {
        Self {
            ok: h.ok,
            version: h.version,
            active_queries: h.active_queries,
            sources_ok: h.sources_ok,
            sources_unavailable: h.sources_unavailable,
        }
    }
}

// ---- QueryRequest ----

impl From<core::QueryRequest> for v1::QueryRequest {
    fn from(req: core::QueryRequest) -> Self {
        Self {
            sql: req.sql,
            params: req.params,
            timeout: req.timeout.and_then(|d| {
                Some(prost_types::Duration {
                    seconds: i64::try_from(d.as_secs()).ok()?,
                    nanos: i32::try_from(d.subsec_nanos()).ok()?,
                })
            }),
            max_rows: req.max_rows,
            trace_id: req.trace_id.unwrap_or_default(),
        }
    }
}

impl From<v1::QueryRequest> for core::QueryRequest {
    fn from(req: v1::QueryRequest) -> Self {
        Self {
            sql: req.sql,
            params: req.params,
            timeout: req.timeout.and_then(|d| {
                let secs = u64::try_from(d.seconds).ok()?;
                let nanos = u32::try_from(d.nanos).ok()?;
                Some(std::time::Duration::new(secs, nanos))
            }),
            max_rows: req.max_rows,
            trace_id: if req.trace_id.is_empty() {
                None
            } else {
                Some(req.trace_id)
            },
        }
    }
}

// ---- Status <-> EngineError ----

/// Map a core `EngineError` to a `tonic::Status` carrying the stable error code in metadata.
#[must_use]
pub fn engine_error_to_status(err: &core::EngineError) -> tonic::Status {
    let code = err.code();
    let mut status = tonic::Status::new(grpc_code(err), err.to_string());
    if let Ok(value) = code.parse() {
        status.metadata_mut().insert("pawrly-error-code", value);
    }
    status
}

/// Inverse mapping for use by clients.
#[must_use]
pub fn status_to_engine_error(status: tonic::Status) -> core::EngineError {
    let code = status
        .metadata()
        .get("pawrly-error-code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("PAWRLY_INTERNAL");
    let msg = status.message().to_string();
    match code {
        "PAWRLY_CANCELLED" => core::EngineError::Cancelled,
        "PAWRLY_TIMEOUT" => core::EngineError::Timeout(std::time::Duration::ZERO),
        "PAWRLY_OOM" => core::EngineError::OutOfMemory(0),
        "PAWRLY_UNKNOWN_TABLE" => core::EngineError::UnknownTable(msg),
        "PAWRLY_UNKNOWN_KIND" => core::EngineError::UnknownKind(msg),
        "PAWRLY_INVALID_SQL" => core::EngineError::InvalidSql(msg),
        "PAWRLY_PROTOCOL" => core::EngineError::Protocol(msg),
        _ => core::EngineError::Internal(format!("{code}: {msg}")),
    }
}

fn grpc_code(err: &core::EngineError) -> tonic::Code {
    match err {
        core::EngineError::UnknownKind(_)
        | core::EngineError::UnknownTable(_)
        | core::EngineError::InvalidSql(_) => tonic::Code::InvalidArgument,
        core::EngineError::Safety(_) | core::EngineError::SourceRegistration { .. } => {
            tonic::Code::FailedPrecondition
        }
        core::EngineError::Timeout(_) => tonic::Code::DeadlineExceeded,
        core::EngineError::OutOfMemory(_) => tonic::Code::ResourceExhausted,
        core::EngineError::Cancelled => tonic::Code::Cancelled,
        core::EngineError::Protocol(_) | core::EngineError::Internal(_) => tonic::Code::Internal,
    }
}

// ---- helpers ----

fn timestamp_from(t: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: t.timestamp(),
        nanos: t.timestamp_subsec_nanos() as i32,
    }
}

fn timestamp_to(t: Timestamp) -> DateTime<Utc> {
    Utc.timestamp_opt(t.seconds, t.nanos as u32)
        .single()
        .unwrap_or_else(Utc::now)
}

#[derive(Debug, thiserror::Error)]
pub enum ConvError {
    #[error("required field `{0}` was missing")]
    Missing(&'static str),

    #[error("invalid value for field `{0}`")]
    Invalid(&'static str),

    #[error("source kind UNSPECIFIED is not a valid runtime value")]
    UnspecifiedSourceKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_kind_round_trip() {
        for k in [
            core::SourceKind::Http,
            core::SourceKind::Github,
            core::SourceKind::Snowflake,
            core::SourceKind::Iceberg,
            core::SourceKind::Ai,
        ] {
            let proto: v1::SourceKind = k.into();
            let back: core::SourceKind = proto.try_into().unwrap();
            assert_eq!(back, k);
        }
    }

    #[test]
    fn unspecified_source_kind_errs() {
        let r: Result<core::SourceKind, _> = v1::SourceKind::Unspecified.try_into();
        assert!(r.is_err());
    }

    #[test]
    fn table_name_round_trip() {
        let n = core::TableName::new("gh", "pulls");
        let proto: v1::TableName = (&n).into();
        let back: core::TableName = proto.into();
        assert_eq!(back, n);
    }

    #[test]
    fn query_request_round_trip() {
        let req = core::QueryRequest {
            sql: "SELECT 1".into(),
            params: [("k".into(), "v".into())].into_iter().collect(),
            timeout: Some(std::time::Duration::from_secs(30)),
            max_rows: 100,
            trace_id: Some("abc".into()),
        };
        let proto: v1::QueryRequest = req.clone().into();
        let back: core::QueryRequest = proto.into();
        assert_eq!(back.sql, req.sql);
        assert_eq!(back.timeout, req.timeout);
        assert_eq!(back.max_rows, req.max_rows);
        assert_eq!(back.trace_id, req.trace_id);
    }
}
