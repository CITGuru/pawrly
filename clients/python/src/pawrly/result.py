"""Result types shared by every transport."""

from dataclasses import dataclass, field


@dataclass
class QueryResult:
    columns: list[str] = field(default_factory=list)
    rows: list[dict] = field(default_factory=list)
    row_count: int = 0
    #: True when rows existed beyond the limit; always ``False`` over REST
    #: (NDJSON carries no completion envelope).
    truncated: bool = False


@dataclass
class HealthReport:
    ok: bool = False
    version: str = ""


@dataclass
class MaterializeSpec:
    """Currently only the ``query`` origin (persist a SQL result)."""

    sql: str
    params: dict[str, str] | None = None
    kind: str = "query"


@dataclass
class MaterializeOutcome:
    name: dict = field(default_factory=dict)  # {"schema": ..., "table": ...}
    file_path: str = ""
    row_count: int = 0
    size_bytes: int = 0


@dataclass
class SemanticFilter:
    member: str
    #: one of equals/not_equals/in/not_in/gt/gte/lt/lte/in_range/contains/
    #: starts_with/ends_with/is_null/is_not_null
    op: str
    values: list[str] | None = None


@dataclass
class SemanticOrder:
    member: str
    desc: bool = False


@dataclass
class SemanticQuery:
    measures: list[str] | None = None
    dimensions: list[str] | None = None
    filters: list[SemanticFilter] | None = None
    order_by: list[SemanticOrder] | None = None
    segments: list[str] | None = None
    limit: int | None = None
    time_zone: str | None = None
    params: dict[str, str] | None = None


@dataclass
class TableName:
    schema: str = ""
    table: str = ""


@dataclass
class SourceInfo:
    name: str = ""
    #: file | http | mcp | postgres | mysql | sqlite | duckdb | snowflake | iceberg | ducklake | delta
    kind: str = ""
    #: ok | unavailable
    status: str = ""
    status_detail: str | None = None
    sub_kind: str | None = None
    table_count: int = 0
    registered_at: str = ""


@dataclass
class TableInfo:
    name: TableName = field(default_factory=TableName)
    kind: str = ""
    description: str | None = None
    row_count_estimate: int | None = None
    cached: bool = False
    required_filters: list[str] = field(default_factory=list)


@dataclass
class ColumnSpec:
    name: str = ""
    #: Arrow type as a string, e.g. "Int64", "Decimal128(18, 2)".
    data_type: str = ""
    nullable: bool = False
    description: str | None = None
    is_filter_pushable: bool = False
    is_required_filter: bool = False


@dataclass
class TableDescription:
    table: TableInfo = field(default_factory=TableInfo)
    columns: list[ColumnSpec] = field(default_factory=list)
    pushable_filter_columns: list[str] = field(default_factory=list)
    examples: list[str] = field(default_factory=list)
    wiki: str | None = None


@dataclass
class TableSummary:
    name: str = ""
    #: single-line "col1 type, col2 type, ..." form.
    columns: str = ""
    required_filters: list[str] = field(default_factory=list)


@dataclass
class SchemaSummary:
    name: str = ""
    kind: str = ""
    tables: list[TableSummary] = field(default_factory=list)


@dataclass
class CatalogSnapshot:
    schemas: list[SchemaSummary] = field(default_factory=list)


@dataclass
class CacheEntryInfo:
    name: TableName = field(default_factory=TableName)
    #: none | ttl | refresh | cron | append
    mode: str = ""
    written_at: str = ""
    expires_at: str | None = None
    row_count: int = 0
    size_bytes: int = 0
    file_count: int = 0


@dataclass
class FunctionInfo:
    namespace: str = ""
    name: str = ""
    kind: str = ""
    builtin: bool = False
    signature: str = ""
    description: str | None = None


@dataclass
class FunctionArg:
    name: str = ""
    type: str = ""
    required: bool = False
    default: str | None = None
    description: str | None = None
    tool_arg: str | None = None


@dataclass
class FunctionColumn:
    name: str = ""
    type: str = ""
    source: str | None = None
    description: str | None = None


@dataclass
class FunctionDescription:
    namespace: str = ""
    name: str = ""
    kind: str = ""
    builtin: bool = False
    signature: str = ""
    description: str | None = None
    wiki: str | None = None
    examples: list[str] = field(default_factory=list)
    args: list[FunctionArg] = field(default_factory=list)
    returns: list[FunctionColumn] = field(default_factory=list)


@dataclass
class SemanticModelInfo:
    name: str = ""
    description: str | None = None
    source: str = ""
    dimension_count: int = 0
    measure_count: int = 0


@dataclass
class SourceTestReport:
    name: str = ""
    ok: bool = False
    latency_ms: float = 0.0
    detail: str | None = None


@dataclass
class ReloadReport:
    sources_added: int = 0
    sources_removed: int = 0
    sources_changed: int = 0


@dataclass
class RefreshCatalogOutcome:
    sources_refreshed: int = 0
    tables_discovered: int = 0


@dataclass
class RefreshOutcome:
    table: TableName = field(default_factory=TableName)
    rows_written: int = 0
    size_bytes: int = 0
    elapsed_ms: float = 0.0
    expires_at: str | None = None


@dataclass
class VacuumReport:
    entries_removed: int = 0
    files_removed: int = 0
    bytes_reclaimed: int = 0


@dataclass
class SemanticDimension:
    name: str = ""
    expr: str = ""
    #: string | number | time | bool
    type: str = ""
    #: time grains (hour | day | week | month | quarter | year); only for `time`
    grains: list[str] = field(default_factory=list)
    description: str | None = None


@dataclass
class SemanticMeasure:
    name: str = ""
    #: sum | count | count_distinct | avg | min | max | custom
    agg: str = ""
    #: the SQL when ``agg == "custom"``
    custom_sql: str | None = None
    expr: str = ""
    filters: list[str] = field(default_factory=list)
    format: str | None = None
    description: str | None = None


@dataclass
class SemanticRelationship:
    name: str = ""
    #: many_to_one | one_to_many | one_to_one
    kind: str = ""
    target: str = ""
    on: str = ""


@dataclass
class SemanticSegment:
    name: str = ""
    description: str | None = None
    filters: list[SemanticFilter] = field(default_factory=list)


@dataclass
class SemanticModelDescription:
    name: str = ""
    description: str | None = None
    source: str = ""
    primary_key: list[str] = field(default_factory=list)
    dimensions: list[SemanticDimension] = field(default_factory=list)
    measures: list[SemanticMeasure] = field(default_factory=list)
    relationships: list[SemanticRelationship] = field(default_factory=list)
    segments: list[SemanticSegment] = field(default_factory=list)
