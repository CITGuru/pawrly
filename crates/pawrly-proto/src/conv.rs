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
            core::SourceKind::File => Self::File,
            core::SourceKind::Mcp => Self::Mcp,
            core::SourceKind::Sqlite => Self::Sqlite,
            core::SourceKind::Postgres => Self::Postgres,
            core::SourceKind::Mysql => Self::Mysql,
            core::SourceKind::Duckdb => Self::Duckdb,
            core::SourceKind::Snowflake => Self::Snowflake,
            core::SourceKind::Iceberg => Self::Iceberg,
            core::SourceKind::Ducklake => Self::Ducklake,
            core::SourceKind::Delta => Self::Delta,
        }
    }
}

impl TryFrom<v1::SourceKind> for core::SourceKind {
    type Error = ConvError;

    fn try_from(k: v1::SourceKind) -> Result<Self, ConvError> {
        match k {
            v1::SourceKind::Unspecified => Err(ConvError::UnspecifiedSourceKind),
            v1::SourceKind::Http => Ok(Self::Http),
            v1::SourceKind::File => Ok(Self::File),
            v1::SourceKind::Mcp => Ok(Self::Mcp),
            v1::SourceKind::Sqlite => Ok(Self::Sqlite),
            v1::SourceKind::Postgres => Ok(Self::Postgres),
            v1::SourceKind::Mysql => Ok(Self::Mysql),
            v1::SourceKind::Duckdb => Ok(Self::Duckdb),
            v1::SourceKind::Snowflake => Ok(Self::Snowflake),
            v1::SourceKind::Iceberg => Ok(Self::Iceberg),
            v1::SourceKind::Ducklake => Ok(Self::Ducklake),
            v1::SourceKind::Delta => Ok(Self::Delta),
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

// ---- MaterializeSpec / MaterializeOutcome ----

/// Map a core format to the proto enum's i32 discriminant.
fn format_to_proto(f: core::MaterializeFormat) -> i32 {
    match f {
        core::MaterializeFormat::Parquet => v1::MaterializeFormat::Parquet as i32,
        core::MaterializeFormat::Csv => v1::MaterializeFormat::Csv as i32,
        core::MaterializeFormat::Json => v1::MaterializeFormat::Json as i32,
    }
}

/// Map a proto format i32 back to a core format. `UNSPECIFIED`/unknown → `None`
/// (infer from the extension).
fn format_from_proto(v: i32) -> Option<core::MaterializeFormat> {
    match v1::MaterializeFormat::try_from(v).ok()? {
        v1::MaterializeFormat::Parquet => Some(core::MaterializeFormat::Parquet),
        v1::MaterializeFormat::Csv => Some(core::MaterializeFormat::Csv),
        v1::MaterializeFormat::Json => Some(core::MaterializeFormat::Json),
        v1::MaterializeFormat::Unspecified => None,
    }
}

/// An optional format encodes as `UNSPECIFIED` when absent.
fn opt_format_to_proto(f: Option<core::MaterializeFormat>) -> i32 {
    f.map_or(v1::MaterializeFormat::Unspecified as i32, format_to_proto)
}

impl From<core::MaterializeSpec> for v1::MaterializeSpec {
    fn from(s: core::MaterializeSpec) -> Self {
        use core::MaterializeSpec as S;
        let spec = match s {
            S::Query { sql, params } => {
                v1::materialize_spec::Spec::Query(v1::QuerySpec { sql, params })
            }
            S::File { path, format } => v1::materialize_spec::Spec::File(v1::FileSpec {
                path: path.to_string_lossy().into_owned(),
                format: opt_format_to_proto(format),
            }),
            S::Url { url, format } => v1::materialize_spec::Spec::Url(v1::UrlSpec {
                url,
                format: opt_format_to_proto(format),
            }),
            S::Inline { bytes, format } => v1::materialize_spec::Spec::Inline(v1::InlineSpec {
                bytes,
                format: format_to_proto(format),
            }),
        };
        Self { spec: Some(spec) }
    }
}

impl TryFrom<v1::MaterializeSpec> for core::MaterializeSpec {
    type Error = tonic::Status;

    fn try_from(s: v1::MaterializeSpec) -> Result<Self, Self::Error> {
        use v1::materialize_spec::Spec;
        match s.spec {
            Some(Spec::Query(q)) => Ok(core::MaterializeSpec::Query {
                sql: q.sql,
                params: q.params,
            }),
            Some(Spec::File(f)) => Ok(core::MaterializeSpec::File {
                path: f.path.into(),
                format: format_from_proto(f.format),
            }),
            Some(Spec::Url(u)) => Ok(core::MaterializeSpec::Url {
                url: u.url,
                format: format_from_proto(u.format),
            }),
            Some(Spec::Inline(i)) => Ok(core::MaterializeSpec::Inline {
                bytes: i.bytes,
                format: format_from_proto(i.format).ok_or_else(|| {
                    tonic::Status::invalid_argument("inline materialize requires a format")
                })?,
            }),
            None => Err(tonic::Status::invalid_argument(
                "materialize spec is required",
            )),
        }
    }
}

impl From<core::MaterializeOutcome> for v1::MaterializeResponse {
    fn from(o: core::MaterializeOutcome) -> Self {
        Self {
            name: Some(o.name.into()),
            file_path: o.file_path.to_string_lossy().into_owned(),
            row_count: o.row_count,
            size_bytes: o.size_bytes,
        }
    }
}

impl From<v1::MaterializeResponse> for core::MaterializeOutcome {
    fn from(r: v1::MaterializeResponse) -> Self {
        Self {
            name: r
                .name
                .map(Into::into)
                .unwrap_or_else(|| core::TableName::new(core::MATERIALIZED_SCHEMA, String::new())),
            file_path: std::path::PathBuf::from(r.file_path),
            row_count: r.row_count,
            size_bytes: r.size_bytes,
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
            sub_kind: s.sub_kind,
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
            sub_kind: s.sub_kind,
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
            // Interface/principal are determined at the server boundary (the gRPC
            // service fills them), not carried on the wire; default here.
            context: core::activity::RequestContext::default(),
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
        "PAWRLY_SEMANTIC_PLAN" => core::EngineError::SemanticPlan(msg),
        "PAWRLY_PROTOCOL" => core::EngineError::Protocol(msg),
        _ => core::EngineError::Internal(format!("{code}: {msg}")),
    }
}

fn grpc_code(err: &core::EngineError) -> tonic::Code {
    match err {
        core::EngineError::UnknownKind(_)
        | core::EngineError::UnknownTable(_)
        | core::EngineError::UnknownFunction(_)
        | core::EngineError::InvalidSql(_)
        | core::EngineError::SemanticPlan(_) => tonic::Code::InvalidArgument,
        core::EngineError::Safety(_) | core::EngineError::SourceRegistration { .. } => {
            tonic::Code::FailedPrecondition
        }
        core::EngineError::Timeout(_) => tonic::Code::DeadlineExceeded,
        core::EngineError::OutOfMemory(_) => tonic::Code::ResourceExhausted,
        core::EngineError::Cancelled => tonic::Code::Cancelled,
        core::EngineError::Protocol(_) | core::EngineError::Internal(_) => tonic::Code::Internal,
    }
}

// ---- semantic ----

impl From<core::semantic::DimensionType> for v1::DimensionType {
    fn from(t: core::semantic::DimensionType) -> Self {
        match t {
            core::semantic::DimensionType::String => Self::String,
            core::semantic::DimensionType::Number => Self::Number,
            core::semantic::DimensionType::Time => Self::Time,
            core::semantic::DimensionType::Bool => Self::Bool,
        }
    }
}

impl TryFrom<v1::DimensionType> for core::semantic::DimensionType {
    type Error = ConvError;

    fn try_from(t: v1::DimensionType) -> Result<Self, ConvError> {
        match t {
            v1::DimensionType::Unspecified => Err(ConvError::Invalid("DimensionType")),
            v1::DimensionType::String => Ok(Self::String),
            v1::DimensionType::Number => Ok(Self::Number),
            v1::DimensionType::Time => Ok(Self::Time),
            v1::DimensionType::Bool => Ok(Self::Bool),
        }
    }
}

impl From<core::semantic::TimeGrain> for v1::TimeGrain {
    fn from(g: core::semantic::TimeGrain) -> Self {
        match g {
            core::semantic::TimeGrain::Hour => Self::Hour,
            core::semantic::TimeGrain::Day => Self::Day,
            core::semantic::TimeGrain::Week => Self::Week,
            core::semantic::TimeGrain::Month => Self::Month,
            core::semantic::TimeGrain::Quarter => Self::Quarter,
            core::semantic::TimeGrain::Year => Self::Year,
        }
    }
}

impl TryFrom<v1::TimeGrain> for core::semantic::TimeGrain {
    type Error = ConvError;

    fn try_from(g: v1::TimeGrain) -> Result<Self, ConvError> {
        match g {
            v1::TimeGrain::Unspecified => Err(ConvError::Invalid("TimeGrain")),
            v1::TimeGrain::Hour => Ok(Self::Hour),
            v1::TimeGrain::Day => Ok(Self::Day),
            v1::TimeGrain::Week => Ok(Self::Week),
            v1::TimeGrain::Month => Ok(Self::Month),
            v1::TimeGrain::Quarter => Ok(Self::Quarter),
            v1::TimeGrain::Year => Ok(Self::Year),
        }
    }
}

impl From<core::semantic::FilterOp> for v1::FilterOp {
    fn from(op: core::semantic::FilterOp) -> Self {
        use core::semantic::FilterOp as F;
        match op {
            F::Equals => Self::Equals,
            F::NotEquals => Self::NotEquals,
            F::In => Self::In,
            F::NotIn => Self::NotIn,
            F::Gt => Self::Gt,
            F::Gte => Self::Gte,
            F::Lt => Self::Lt,
            F::Lte => Self::Lte,
            F::InRange => Self::InRange,
            F::Contains => Self::Contains,
            F::StartsWith => Self::StartsWith,
            F::EndsWith => Self::EndsWith,
            F::IsNull => Self::IsNull,
            F::IsNotNull => Self::IsNotNull,
        }
    }
}

impl TryFrom<v1::FilterOp> for core::semantic::FilterOp {
    type Error = ConvError;

    fn try_from(op: v1::FilterOp) -> Result<Self, ConvError> {
        use core::semantic::FilterOp as F;
        Ok(match op {
            v1::FilterOp::Unspecified => return Err(ConvError::Invalid("FilterOp")),
            v1::FilterOp::Equals => F::Equals,
            v1::FilterOp::NotEquals => F::NotEquals,
            v1::FilterOp::In => F::In,
            v1::FilterOp::NotIn => F::NotIn,
            v1::FilterOp::Gt => F::Gt,
            v1::FilterOp::Gte => F::Gte,
            v1::FilterOp::Lt => F::Lt,
            v1::FilterOp::Lte => F::Lte,
            v1::FilterOp::InRange => F::InRange,
            v1::FilterOp::Contains => F::Contains,
            v1::FilterOp::StartsWith => F::StartsWith,
            v1::FilterOp::EndsWith => F::EndsWith,
            v1::FilterOp::IsNull => F::IsNull,
            v1::FilterOp::IsNotNull => F::IsNotNull,
        })
    }
}

impl From<core::semantic::Dimension> for v1::Dimension {
    fn from(d: core::semantic::Dimension) -> Self {
        Self {
            name: d.name,
            expr: d.expr,
            r#type: v1::DimensionType::from(d.data_type) as i32,
            grains: d
                .time_grains
                .into_iter()
                .map(|g| v1::TimeGrain::from(g) as i32)
                .collect(),
            description: d.description.unwrap_or_default(),
        }
    }
}

impl TryFrom<v1::Dimension> for core::semantic::Dimension {
    type Error = ConvError;

    fn try_from(d: v1::Dimension) -> Result<Self, ConvError> {
        let data_type = v1::DimensionType::try_from(d.r#type)
            .map_err(|_| ConvError::Invalid("Dimension.type"))?
            .try_into()?;
        let mut time_grains = Vec::with_capacity(d.grains.len());
        for g in d.grains {
            let grain = v1::TimeGrain::try_from(g)
                .map_err(|_| ConvError::Invalid("Dimension.grains"))?
                .try_into()?;
            time_grains.push(grain);
        }
        Ok(Self {
            name: d.name,
            expr: d.expr,
            data_type,
            time_grains,
            description: if d.description.is_empty() {
                None
            } else {
                Some(d.description)
            },
        })
    }
}

impl From<core::semantic::Measure> for v1::Measure {
    fn from(m: core::semantic::Measure) -> Self {
        let agg = m.agg.label().to_string();
        let custom_sql = match &m.agg {
            core::semantic::MeasureAgg::Custom { sql } => sql.clone(),
            _ => String::new(),
        };
        Self {
            name: m.name,
            agg,
            expr: m.expr,
            filters: m.filters,
            format: m.format.unwrap_or_default(),
            description: m.description.unwrap_or_default(),
            custom_sql,
        }
    }
}

impl TryFrom<v1::Measure> for core::semantic::Measure {
    type Error = ConvError;

    fn try_from(m: v1::Measure) -> Result<Self, ConvError> {
        use core::semantic::MeasureAgg as A;
        let agg = match m.agg.as_str() {
            "sum" => A::Sum,
            "count" => A::Count,
            "count_distinct" => A::CountDistinct,
            "avg" => A::Avg,
            "min" => A::Min,
            "max" => A::Max,
            "custom" => A::Custom { sql: m.custom_sql },
            _ => return Err(ConvError::Invalid("Measure.agg")),
        };
        Ok(Self {
            name: m.name,
            agg,
            expr: m.expr,
            filters: m.filters,
            format: if m.format.is_empty() {
                None
            } else {
                Some(m.format)
            },
            description: if m.description.is_empty() {
                None
            } else {
                Some(m.description)
            },
        })
    }
}

impl From<core::semantic::SemanticModelInfo> for v1::ModelInfo {
    fn from(m: core::semantic::SemanticModelInfo) -> Self {
        Self {
            name: m.name,
            description: m.description.unwrap_or_default(),
            source: m.source,
            dimension_count: m.dimension_count,
            measure_count: m.measure_count,
        }
    }
}

impl From<v1::ModelInfo> for core::semantic::SemanticModelInfo {
    fn from(m: v1::ModelInfo) -> Self {
        Self {
            name: m.name,
            description: if m.description.is_empty() {
                None
            } else {
                Some(m.description)
            },
            source: m.source,
            dimension_count: m.dimension_count,
            measure_count: m.measure_count,
        }
    }
}

impl From<core::semantic::RelationshipKind> for v1::RelationshipKind {
    fn from(k: core::semantic::RelationshipKind) -> Self {
        use core::semantic::RelationshipKind as K;
        match k {
            K::ManyToOne => Self::ManyToOne,
            K::OneToMany => Self::OneToMany,
            K::OneToOne => Self::OneToOne,
        }
    }
}

impl TryFrom<v1::RelationshipKind> for core::semantic::RelationshipKind {
    type Error = ConvError;

    fn try_from(k: v1::RelationshipKind) -> Result<Self, ConvError> {
        use core::semantic::RelationshipKind as K;
        Ok(match k {
            v1::RelationshipKind::Unspecified => {
                return Err(ConvError::Invalid("RelationshipKind"));
            }
            v1::RelationshipKind::ManyToOne => K::ManyToOne,
            v1::RelationshipKind::OneToMany => K::OneToMany,
            v1::RelationshipKind::OneToOne => K::OneToOne,
        })
    }
}

impl From<core::semantic::Relationship> for v1::Relationship {
    fn from(r: core::semantic::Relationship) -> Self {
        Self {
            name: r.name,
            kind: v1::RelationshipKind::from(r.kind) as i32,
            target: r.target_model,
            on: r.join_predicate,
        }
    }
}

impl TryFrom<v1::Relationship> for core::semantic::Relationship {
    type Error = ConvError;

    fn try_from(r: v1::Relationship) -> Result<Self, ConvError> {
        let kind = v1::RelationshipKind::try_from(r.kind)
            .map_err(|_| ConvError::Invalid("Relationship.kind"))?
            .try_into()?;
        Ok(Self {
            name: r.name,
            kind,
            target_model: r.target,
            join_predicate: r.on,
        })
    }
}

impl From<core::semantic::SemanticModelDescription> for v1::ModelDescription {
    fn from(m: core::semantic::SemanticModelDescription) -> Self {
        Self {
            name: m.name,
            description: m.description.unwrap_or_default(),
            source: m.source,
            primary_key: m.primary_key,
            dimensions: m.dimensions.into_iter().map(Into::into).collect(),
            measures: m.measures.into_iter().map(Into::into).collect(),
            relationships: m.relationships.into_iter().map(Into::into).collect(),
            segments: m.segments.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::ModelDescription> for core::semantic::SemanticModelDescription {
    type Error = ConvError;

    fn try_from(m: v1::ModelDescription) -> Result<Self, ConvError> {
        let mut dimensions = Vec::with_capacity(m.dimensions.len());
        for d in m.dimensions {
            dimensions.push(d.try_into()?);
        }
        let mut measures = Vec::with_capacity(m.measures.len());
        for ms in m.measures {
            measures.push(ms.try_into()?);
        }
        let mut relationships = Vec::with_capacity(m.relationships.len());
        for r in m.relationships {
            relationships.push(r.try_into()?);
        }
        Ok(Self {
            name: m.name,
            description: if m.description.is_empty() {
                None
            } else {
                Some(m.description)
            },
            source: m.source,
            primary_key: m.primary_key,
            dimensions,
            measures,
            relationships,
            segments: m.segments.into_iter().map(Into::into).collect(),
        })
    }
}

/// A semantic filter, core → proto.
fn semantic_filter_to_proto(f: core::semantic::SemanticFilter) -> v1::SemanticFilter {
    v1::SemanticFilter {
        member: f.member,
        op: v1::FilterOp::from(f.op) as i32,
        values: f.values,
    }
}

/// A semantic filter, proto → core. An unrecognized/unspecified op defaults to
/// `Equals` rather than failing an infallible conversion.
fn semantic_filter_from_proto(f: v1::SemanticFilter) -> core::semantic::SemanticFilter {
    core::semantic::SemanticFilter {
        member: f.member,
        op: v1::FilterOp::try_from(f.op)
            .ok()
            .and_then(|op| core::semantic::FilterOp::try_from(op).ok())
            .unwrap_or(core::semantic::FilterOp::Equals),
        values: f.values,
    }
}

impl From<core::semantic::Segment> for v1::Segment {
    fn from(s: core::semantic::Segment) -> Self {
        Self {
            name: s.name,
            description: s.description.unwrap_or_default(),
            filters: s
                .filters
                .into_iter()
                .map(semantic_filter_to_proto)
                .collect(),
        }
    }
}

impl From<v1::Segment> for core::semantic::Segment {
    fn from(s: v1::Segment) -> Self {
        Self {
            name: s.name,
            description: if s.description.is_empty() {
                None
            } else {
                Some(s.description)
            },
            filters: s
                .filters
                .into_iter()
                .map(semantic_filter_from_proto)
                .collect(),
        }
    }
}

impl From<core::semantic::SemanticQuery> for v1::SemanticQueryRequest {
    fn from(q: core::semantic::SemanticQuery) -> Self {
        Self {
            measures: q.measures,
            dimensions: q.dimensions,
            filters: q
                .filters
                .into_iter()
                .map(semantic_filter_to_proto)
                .collect(),
            segments: q.segments,
            order_by: q
                .order_by
                .into_iter()
                .map(|o| v1::SemanticOrder {
                    member: o.member,
                    desc: matches!(o.direction, core::semantic::OrderDir::Desc),
                })
                .collect(),
            limit: q.limit,
            time_zone: q.time_zone,
            params: q.params,
            timeout: None,
            trace_id: String::new(),
        }
    }
}

impl From<v1::SemanticQueryRequest> for core::semantic::SemanticQuery {
    fn from(q: v1::SemanticQueryRequest) -> Self {
        let filters = q
            .filters
            .into_iter()
            .map(semantic_filter_from_proto)
            .collect();
        let order_by = q
            .order_by
            .into_iter()
            .map(|o| core::semantic::SemanticOrder {
                member: o.member,
                direction: if o.desc {
                    core::semantic::OrderDir::Desc
                } else {
                    core::semantic::OrderDir::Asc
                },
            })
            .collect();
        Self {
            measures: q.measures,
            dimensions: q.dimensions,
            filters,
            segments: q.segments,
            order_by,
            limit: q.limit,
            time_zone: q.time_zone,
            params: q.params,
        }
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

// ---- functions ----

fn function_kind_str(k: core::FunctionKind) -> String {
    k.as_str().to_string()
}

fn parse_function_kind(s: &str) -> core::FunctionKind {
    match s {
        "mcp" => core::FunctionKind::Mcp,
        "file" => core::FunctionKind::File,
        _ => core::FunctionKind::Http,
    }
}

impl From<core::FunctionInfo> for v1::FunctionInfo {
    fn from(f: core::FunctionInfo) -> Self {
        Self {
            namespace: f.namespace,
            name: f.name,
            kind: function_kind_str(f.kind),
            builtin: f.builtin,
            signature: f.signature,
            description: f.description,
        }
    }
}

impl From<v1::FunctionInfo> for core::FunctionInfo {
    fn from(f: v1::FunctionInfo) -> Self {
        Self {
            namespace: f.namespace,
            name: f.name,
            kind: parse_function_kind(&f.kind),
            builtin: f.builtin,
            signature: f.signature,
            description: f.description,
        }
    }
}

impl From<core::FunctionArg> for v1::FunctionArg {
    fn from(a: core::FunctionArg) -> Self {
        Self {
            name: a.name,
            r#type: a.r#type,
            required: a.required,
            default: a.default,
            description: a.description,
            tool_arg: a.tool_arg,
        }
    }
}

impl From<v1::FunctionArg> for core::FunctionArg {
    fn from(a: v1::FunctionArg) -> Self {
        Self {
            name: a.name,
            r#type: a.r#type,
            required: a.required,
            default: a.default,
            description: a.description,
            tool_arg: a.tool_arg,
        }
    }
}

impl From<core::FunctionColumn> for v1::FunctionColumn {
    fn from(c: core::FunctionColumn) -> Self {
        Self {
            name: c.name,
            r#type: c.r#type,
            source: c.source,
            description: c.description,
        }
    }
}

impl From<v1::FunctionColumn> for core::FunctionColumn {
    fn from(c: v1::FunctionColumn) -> Self {
        Self {
            name: c.name,
            r#type: c.r#type,
            source: c.source,
            description: c.description,
        }
    }
}

impl From<core::FunctionDescription> for v1::FunctionDescription {
    fn from(d: core::FunctionDescription) -> Self {
        Self {
            namespace: d.namespace,
            name: d.name,
            kind: function_kind_str(d.kind),
            builtin: d.builtin,
            signature: d.signature,
            description: d.description,
            wiki: d.wiki,
            examples: d.examples,
            args: d.args.into_iter().map(Into::into).collect(),
            returns: d.returns.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<v1::FunctionDescription> for core::FunctionDescription {
    fn from(d: v1::FunctionDescription) -> Self {
        Self {
            namespace: d.namespace,
            name: d.name,
            kind: parse_function_kind(&d.kind),
            builtin: d.builtin,
            signature: d.signature,
            description: d.description,
            wiki: d.wiki,
            examples: d.examples,
            args: d.args.into_iter().map(Into::into).collect(),
            returns: d.returns.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_kind_round_trip() {
        for k in [
            core::SourceKind::Http,
            core::SourceKind::File,
            core::SourceKind::Snowflake,
            core::SourceKind::Iceberg,
            core::SourceKind::Duckdb,
            core::SourceKind::Ducklake,
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
            context: core::activity::RequestContext::default(),
        };
        let proto: v1::QueryRequest = req.clone().into();
        let back: core::QueryRequest = proto.into();
        assert_eq!(back.sql, req.sql);
        assert_eq!(back.timeout, req.timeout);
        assert_eq!(back.max_rows, req.max_rows);
        assert_eq!(back.trace_id, req.trace_id);
    }
}
