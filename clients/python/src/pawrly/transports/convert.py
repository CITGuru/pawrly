"""Build result dataclasses from the REST JSON shapes (snake_case, 1:1 with the
engine's serde types)."""

import re

from ..result import (
    CacheEntryInfo,
    CatalogSnapshot,
    ColumnSpec,
    FunctionArg,
    FunctionColumn,
    FunctionDescription,
    FunctionInfo,
    RefreshCatalogOutcome,
    RefreshOutcome,
    ReloadReport,
    SchemaSummary,
    SemanticDimension,
    SemanticFilter,
    SemanticMeasure,
    SemanticModelDescription,
    SemanticModelInfo,
    SemanticRelationship,
    SemanticSegment,
    SourceInfo,
    SourceTestReport,
    TableDescription,
    TableInfo,
    TableName,
    TableSummary,
    VacuumReport,
)

_HUMANTIME_MS = {
    "ns": 1e-6,
    "us": 1e-3,
    "µs": 1e-3,
    "ms": 1.0,
    "s": 1000.0,
    "m": 60_000.0,
    "h": 3_600_000.0,
    "d": 86_400_000.0,
}


def humantime_ms(value: str | None) -> float:
    """Parse a humantime duration (`"1s 500ms"`, `"150ms"`) into milliseconds."""
    if not value:
        return 0.0
    total = 0.0
    for num, unit in re.findall(r"(\d+(?:\.\d+)?)\s*(ns|us|µs|ms|s|m|h|d)", value):
        total += float(num) * _HUMANTIME_MS[unit]
    return total


def table_name(d: dict) -> TableName:
    d = d or {}
    return TableName(schema=d.get("schema", ""), table=d.get("table", ""))


def source_info(d: dict) -> SourceInfo:
    return SourceInfo(
        name=d.get("name", ""),
        kind=d.get("kind", ""),
        status=d.get("status", ""),
        status_detail=d.get("status_detail"),
        sub_kind=d.get("sub_kind"),
        table_count=d.get("table_count", 0),
        registered_at=d.get("registered_at", ""),
    )


def table_info(d: dict) -> TableInfo:
    return TableInfo(
        name=table_name(d.get("name")),
        kind=d.get("kind", ""),
        description=d.get("description"),
        row_count_estimate=d.get("row_count_estimate"),
        cached=d.get("cached", False),
        required_filters=d.get("required_filters", []),
    )


def column_spec(d: dict) -> ColumnSpec:
    return ColumnSpec(
        name=d.get("name", ""),
        data_type=d.get("data_type", ""),
        nullable=d.get("nullable", False),
        description=d.get("description"),
        is_filter_pushable=d.get("is_filter_pushable", False),
        is_required_filter=d.get("is_required_filter", False),
    )


def table_description(d: dict) -> TableDescription:
    return TableDescription(
        table=table_info(d.get("table")),
        columns=[column_spec(c) for c in d.get("columns", [])],
        pushable_filter_columns=d.get("pushable_filter_columns", []),
        examples=d.get("examples", []),
        wiki=d.get("wiki"),
    )


def catalog_snapshot(d: dict) -> CatalogSnapshot:
    return CatalogSnapshot(
        schemas=[
            SchemaSummary(
                name=s.get("name", ""),
                kind=s.get("kind", ""),
                tables=[
                    TableSummary(
                        name=t.get("name", ""),
                        columns=t.get("columns", ""),
                        required_filters=t.get("required_filters", []),
                    )
                    for t in s.get("tables", [])
                ],
            )
            for s in d.get("schemas", [])
        ]
    )


def cache_entry(d: dict) -> CacheEntryInfo:
    return CacheEntryInfo(
        name=table_name(d.get("name")),
        mode=d.get("mode", ""),
        written_at=d.get("written_at", ""),
        expires_at=d.get("expires_at"),
        row_count=d.get("row_count", 0),
        size_bytes=d.get("size_bytes", 0),
        file_count=d.get("file_count", 0),
    )


def function_info(d: dict) -> FunctionInfo:
    return FunctionInfo(
        namespace=d.get("namespace", ""),
        name=d.get("name", ""),
        kind=d.get("kind", ""),
        builtin=d.get("builtin", False),
        signature=d.get("signature", ""),
        description=d.get("description"),
    )


def function_description(d: dict) -> FunctionDescription:
    return FunctionDescription(
        namespace=d.get("namespace", ""),
        name=d.get("name", ""),
        kind=d.get("kind", ""),
        builtin=d.get("builtin", False),
        signature=d.get("signature", ""),
        description=d.get("description"),
        wiki=d.get("wiki"),
        examples=d.get("examples", []),
        args=[
            FunctionArg(
                name=a.get("name", ""),
                type=a.get("type", ""),
                required=a.get("required", False),
                default=a.get("default"),
                description=a.get("description"),
                tool_arg=a.get("tool_arg"),
            )
            for a in d.get("args", [])
        ],
        returns=[
            FunctionColumn(
                name=c.get("name", ""),
                type=c.get("type", ""),
                source=c.get("source"),
                description=c.get("description"),
            )
            for c in d.get("returns", [])
        ],
    )


def semantic_model_info(d: dict) -> SemanticModelInfo:
    return SemanticModelInfo(
        name=d.get("name", ""),
        description=d.get("description"),
        source=d.get("source", ""),
        dimension_count=d.get("dimension_count", 0),
        measure_count=d.get("measure_count", 0),
    )


def source_test_report(d: dict) -> SourceTestReport:
    return SourceTestReport(
        name=d.get("name", ""),
        ok=d.get("ok", False),
        latency_ms=humantime_ms(d.get("latency")),
        detail=d.get("detail"),
    )


def reload_report(d: dict) -> ReloadReport:
    return ReloadReport(
        sources_added=d.get("sources_added", 0),
        sources_removed=d.get("sources_removed", 0),
        sources_changed=d.get("sources_changed", 0),
    )


def refresh_catalog_outcome(d: dict) -> RefreshCatalogOutcome:
    return RefreshCatalogOutcome(
        sources_refreshed=d.get("sources_refreshed", 0),
        tables_discovered=d.get("tables_discovered", 0),
    )


def refresh_outcome(d: dict) -> RefreshOutcome:
    return RefreshOutcome(
        table=table_name(d.get("table")),
        rows_written=d.get("rows_written", 0),
        size_bytes=d.get("size_bytes", 0),
        elapsed_ms=humantime_ms(d.get("elapsed")),
        expires_at=d.get("expires_at"),
    )


def vacuum_report(d: dict) -> VacuumReport:
    return VacuumReport(
        entries_removed=d.get("entries_removed", 0),
        files_removed=d.get("files_removed", 0),
        bytes_reclaimed=d.get("bytes_reclaimed", 0),
    )


def _measure_agg(agg) -> tuple[str, str | None]:
    # Unit variants serialize as a bare string; `Custom` as `{custom: {sql}}`.
    if isinstance(agg, str):
        return agg, None
    if isinstance(agg, dict) and "custom" in agg:
        return "custom", (agg["custom"] or {}).get("sql")
    return "", None


def semantic_model_description(d: dict) -> SemanticModelDescription:
    return SemanticModelDescription(
        name=d.get("name", ""),
        description=d.get("description"),
        source=d.get("source", ""),
        primary_key=d.get("primary_key", []),
        dimensions=[
            SemanticDimension(
                name=x.get("name", ""),
                expr=x.get("expr", ""),
                type=x.get("type", ""),
                grains=x.get("grains", []),
                description=x.get("description"),
            )
            for x in d.get("dimensions", [])
        ],
        measures=[_measure(x) for x in d.get("measures", [])],
        relationships=[
            SemanticRelationship(
                name=x.get("name", ""),
                kind=x.get("kind", ""),
                target=x.get("target", ""),
                on=x.get("on", ""),
            )
            for x in d.get("relationships", [])
        ],
        segments=[
            SemanticSegment(
                name=x.get("name", ""),
                description=x.get("description"),
                filters=[
                    SemanticFilter(
                        member=f.get("member", ""),
                        op=f.get("op", ""),
                        values=f.get("values"),
                    )
                    for f in x.get("filters", [])
                ],
            )
            for x in d.get("segments", [])
        ],
    )


def _measure(x: dict) -> SemanticMeasure:
    agg, custom_sql = _measure_agg(x.get("agg"))
    return SemanticMeasure(
        name=x.get("name", ""),
        agg=agg,
        custom_sql=custom_sql,
        expr=x.get("expr", ""),
        filters=x.get("filters", []),
        format=x.get("format"),
        description=x.get("description"),
    )
