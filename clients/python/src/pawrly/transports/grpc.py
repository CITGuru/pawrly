"""``EngineService`` over native gRPC (``pawrly serve``).

Requires the ``grpc`` extra (``pip install pawrly[grpc]``) and the generated
stubs (``scripts/generate.sh``): grpcio + protobuf for the wire, pyarrow to
decode the streamed Arrow IPC batches.
"""

from collections.abc import Iterator

import grpc
import pyarrow as pa

import json

from pawrly.v1 import (
    admin_pb2,
    admin_pb2_grpc,
    cache_pb2,
    cache_pb2_grpc,
    catalog_pb2,
    catalog_pb2_grpc,
    common_pb2,
    query_pb2,
    query_pb2_grpc,
    semantic_pb2,
    semantic_pb2_grpc,
    sources_pb2,
    sources_pb2_grpc,
)

from ..errors import PawrlyError
from ..query import QueryHandle
from ..result import (
    CacheEntryInfo,
    CatalogSnapshot,
    ColumnSpec,
    FunctionArg,
    FunctionColumn,
    FunctionDescription,
    FunctionInfo,
    HealthReport,
    MaterializeOutcome,
    MaterializeSpec,
    RefreshCatalogOutcome,
    RefreshOutcome,
    ReloadReport,
    SemanticDimension,
    SemanticFilter,
    SemanticMeasure,
    SemanticModelDescription,
    SemanticModelInfo,
    SemanticRelationship,
    SemanticSegment,
    SemanticQuery,
    SourceInfo,
    SourceTestReport,
    TableDescription,
    TableInfo,
    TableName,
    VacuumReport,
)
from . import convert

_FILTER_OP = {
    "equals": 1,
    "not_equals": 2,
    "in": 3,
    "not_in": 4,
    "gt": 5,
    "gte": 6,
    "lt": 7,
    "lte": 8,
    "in_range": 9,
    "contains": 10,
    "starts_with": 11,
    "ends_with": 12,
    "is_null": 13,
    "is_not_null": 14,
}


class GrpcTransport:
    name = "grpc"

    def __init__(self, endpoint: str, bearer: str | None = None) -> None:
        target = endpoint.replace("tcp://", "").replace("http://", "")
        self._channel = grpc.insecure_channel(target)
        self._metadata = [("authorization", f"Bearer {bearer}")] if bearer else []
        self._query = query_pb2_grpc.QueryServiceStub(self._channel)
        self._semantic = semantic_pb2_grpc.SemanticServiceStub(self._channel)
        self._cache = cache_pb2_grpc.CacheServiceStub(self._channel)
        self._admin = admin_pb2_grpc.AdminServiceStub(self._channel)
        self._catalog = catalog_pb2_grpc.CatalogServiceStub(self._channel)
        self._sources = sources_pb2_grpc.SourcesServiceStub(self._channel)

    def query(
        self, sql: str, params: dict[str, str], limit: int | None = None
    ) -> QueryHandle:
        req = query_pb2.QueryRequest(sql=sql, params=params, max_rows=limit or 0)
        return self._handle(self._query.Query(req, metadata=self._metadata))

    def semantic_query(self, q: SemanticQuery) -> QueryHandle:
        req = semantic_pb2.SemanticQueryRequest(
            measures=q.measures or [],
            dimensions=q.dimensions or [],
            filters=[
                semantic_pb2.SemanticFilter(
                    member=f.member, op=_FILTER_OP[f.op], values=f.values or []
                )
                for f in (q.filters or [])
            ],
            order_by=[
                semantic_pb2.SemanticOrder(member=o.member, desc=o.desc)
                for o in (q.order_by or [])
            ],
            segments=q.segments or [],
            params=q.params or {},
        )
        if q.limit is not None:
            req.limit = q.limit
        if q.time_zone:
            req.time_zone = q.time_zone
        return self._handle(self._semantic.SemanticQuery(req, metadata=self._metadata))

    def _handle(self, frames) -> QueryHandle:
        it = iter(frames)
        meta = {"columns": [], "row_count": 0, "truncated": False}
        query_id = ""
        pending = None
        try:
            first = next(it)
            if first.WhichOneof("payload") == "started":
                query_id = first.started.query_id
            else:
                pending = first
        except StopIteration:
            pass
        except grpc.RpcError as e:
            raise _status_to_error(e) from None
        return QueryHandle(query_id, _grpc_rows(it, pending, meta), meta)

    def explain(self, sql: str, analyze: bool) -> str:
        return self._unary(
            lambda: self._query.Explain(
                query_pb2.ExplainRequest(sql=sql, analyze=analyze),
                metadata=self._metadata,
            )
        ).plan

    def cancel(self, query_id: str) -> bool:
        return self._unary(
            lambda: self._query.Cancel(
                query_pb2.CancelRequest(query_id=query_id), metadata=self._metadata
            )
        ).cancelled

    def materialize(
        self, name: str, spec: MaterializeSpec, namespace: str | None = None
    ) -> MaterializeOutcome:
        proto_spec = cache_pb2.MaterializeSpec(
            query=cache_pb2.QuerySpec(sql=spec.sql, params=spec.params or {})
        )
        resp = self._unary(
            lambda: self._cache.Materialize(
                cache_pb2.MaterializeRequest(
                    name=name, spec=proto_spec, namespace=namespace or ""
                ),
                metadata=self._metadata,
            )
        )
        _require_namespace_echo(namespace, resp.namespace)
        return MaterializeOutcome(
            name={"schema": resp.name.schema, "table": resp.name.table},
            file_path=resp.file_path,
            row_count=resp.row_count,
            size_bytes=resp.size_bytes,
        )

    def list_sources(self) -> list[SourceInfo]:
        resp = self._unary(
            lambda: self._sources.ListSources(
                sources_pb2.ListSourcesRequest(), metadata=self._metadata
            )
        )
        return [_source_info(s) for s in resp.sources]

    def list_tables(
        self, source: str | None = None, name_glob: str | None = None
    ) -> list[TableInfo]:
        req = catalog_pb2.ListTablesRequest()
        if source is not None:
            req.source = source
        if name_glob is not None:
            req.name_glob = name_glob
        resp = self._unary(
            lambda: self._catalog.ListTables(req, metadata=self._metadata)
        )
        return [_table_info(t) for t in resp.tables]

    def describe_table(self, name: str) -> TableDescription:
        resp = self._unary(
            lambda: self._catalog.DescribeTable(
                catalog_pb2.DescribeTableRequest(name=_table_name_pb(name)),
                metadata=self._metadata,
            )
        )
        return TableDescription(
            table=_table_info(resp.table),
            columns=[_column_spec(c) for c in resp.columns],
            pushable_filter_columns=list(resp.pushable_filter_columns),
            examples=list(resp.examples),
            wiki=resp.wiki if resp.HasField("wiki") else None,
        )

    def schema_snapshot(
        self, sources: list[str] | None = None, compact: bool = False
    ) -> CatalogSnapshot:
        resp = self._unary(
            lambda: self._catalog.SchemaSnapshot(
                catalog_pb2.SchemaSnapshotRequest(
                    sources=sources or [], compact=compact
                ),
                metadata=self._metadata,
            )
        )
        # The snapshot travels as serde JSON bytes — the same shape REST returns.
        return convert.catalog_snapshot(json.loads(resp.snapshot_json))

    def cache_entries(self, namespace: str | None = None) -> list[CacheEntryInfo]:
        resp = self._unary(
            lambda: self._cache.ListEntries(
                cache_pb2.ListEntriesRequest(namespace=namespace or ""),
                metadata=self._metadata,
            )
        )
        _require_namespace_echo(namespace, resp.namespace)
        return [_cache_entry(e) for e in resp.entries]

    def list_functions(self) -> list[FunctionInfo]:
        resp = self._unary(
            lambda: self._catalog.ListFunctions(
                catalog_pb2.ListFunctionsRequest(), metadata=self._metadata
            )
        )
        return [_function_info(f) for f in resp.functions]

    def describe_function(self, namespace: str, name: str) -> FunctionDescription:
        resp = self._unary(
            lambda: self._catalog.DescribeFunction(
                catalog_pb2.DescribeFunctionRequest(namespace=namespace, name=name),
                metadata=self._metadata,
            )
        )
        return _function_description(resp.function)

    def list_semantic_models(self) -> list[SemanticModelInfo]:
        resp = self._unary(
            lambda: self._semantic.ListModels(
                semantic_pb2.ListModelsRequest(), metadata=self._metadata
            )
        )
        return [
            SemanticModelInfo(
                name=m.name,
                description=m.description or None,
                source=m.source,
                dimension_count=m.dimension_count,
                measure_count=m.measure_count,
            )
            for m in resp.models
        ]

    def describe_semantic_model(self, name: str) -> SemanticModelDescription:
        resp = self._unary(
            lambda: self._semantic.DescribeModel(
                semantic_pb2.DescribeModelRequest(name=name), metadata=self._metadata
            )
        )
        return _model_description(resp.model)

    def list_metrics(self) -> list[dict]:
        resp = self._unary(
            lambda: self._semantic.ListMetrics(
                semantic_pb2.ListMetricsRequest(), metadata=self._metadata
            )
        )
        return [json.loads(m) for m in resp.metrics_json]

    def describe_metric(self, name: str) -> dict:
        resp = self._unary(
            lambda: self._semantic.DescribeMetric(
                semantic_pb2.DescribeMetricRequest(name=name), metadata=self._metadata
            )
        )
        return json.loads(resp.metric_json)

    def add_source(self, definition: dict) -> SourceInfo:
        # gRPC AddSource takes YAML; JSON is valid YAML, so serialize the object.
        resp = self._unary(
            lambda: self._sources.AddSource(
                sources_pb2.AddSourceRequest(yaml=json.dumps(definition)),
                metadata=self._metadata,
            )
        )
        return _source_info(resp.source)

    def remove_source(self, name: str) -> bool:
        return self._unary(
            lambda: self._sources.RemoveSource(
                sources_pb2.RemoveSourceRequest(name=name), metadata=self._metadata
            )
        ).removed

    def test_source(self, name: str) -> SourceTestReport:
        resp = self._unary(
            lambda: self._sources.TestSource(
                sources_pb2.TestSourceRequest(name=name), metadata=self._metadata
            )
        )
        return SourceTestReport(
            name=name,
            ok=resp.ok,
            latency_ms=resp.latency.ToNanoseconds() / 1e6,
            detail=resp.detail or None,
        )

    def reload_config(self) -> ReloadReport:
        resp = self._unary(
            lambda: self._sources.ReloadConfig(
                sources_pb2.ReloadConfigRequest(), metadata=self._metadata
            )
        )
        return ReloadReport(
            sources_added=resp.sources_added,
            sources_removed=resp.sources_removed,
            sources_changed=resp.sources_changed,
        )

    def refresh_catalog(self, source: str | None = None) -> RefreshCatalogOutcome:
        req = catalog_pb2.RefreshCatalogRequest()
        if source is not None:
            req.source = source
        resp = self._unary(
            lambda: self._catalog.RefreshCatalog(req, metadata=self._metadata)
        )
        return RefreshCatalogOutcome(
            sources_refreshed=resp.sources_refreshed,
            tables_discovered=resp.tables_discovered,
        )

    def refresh_table(
        self, name: str, namespace: str | None = None
    ) -> RefreshOutcome:
        resp = self._unary(
            lambda: self._cache.Refresh(
                cache_pb2.RefreshRequest(
                    name=_table_name_pb(name), namespace=namespace or ""
                ),
                metadata=self._metadata,
            )
        )
        _require_namespace_echo(namespace, resp.namespace)
        schema, _, table = name.partition(".")
        return RefreshOutcome(
            table=TableName(schema=schema, table=table),
            rows_written=resp.rows_written,
            size_bytes=resp.size_bytes,
            elapsed_ms=resp.elapsed.ToNanoseconds() / 1e6,
            expires_at=resp.expires_at.ToJsonString()
            if resp.HasField("expires_at")
            else None,
        )

    def invalidate_cache(self, name: str) -> bool:
        return self._unary(
            lambda: self._cache.Invalidate(
                cache_pb2.InvalidateRequest(name=_table_name_pb(name)),
                metadata=self._metadata,
            )
        ).removed

    def vacuum_cache(self) -> VacuumReport:
        resp = self._unary(
            lambda: self._cache.Vacuum(
                cache_pb2.VacuumRequest(), metadata=self._metadata
            )
        )
        return VacuumReport(
            entries_removed=resp.entries_removed,
            files_removed=resp.files_removed,
            bytes_reclaimed=resp.bytes_reclaimed,
        )

    def drop_materialized(self, name: str, namespace: str | None = None) -> bool:
        resp = self._unary(
            lambda: self._cache.DropMaterialized(
                cache_pb2.DropMaterializedRequest(
                    name=name, namespace=namespace or ""
                ),
                metadata=self._metadata,
            )
        )
        _require_namespace_echo(namespace, resp.namespace)
        return resp.dropped

    def drop_namespace(self, namespace: str) -> bool:
        resp = self._unary(
            lambda: self._cache.DropNamespace(
                cache_pb2.DropNamespaceRequest(namespace=namespace),
                metadata=self._metadata,
            )
        )
        _require_namespace_echo(namespace, resp.namespace)
        return resp.dropped

    def health(self) -> HealthReport:
        resp = self._unary(
            lambda: self._admin.Health(
                admin_pb2.HealthRequest(), metadata=self._metadata
            )
        )
        return HealthReport(ok=resp.ok, version=resp.version)

    def shutdown(self) -> None:
        self._unary(
            lambda: self._admin.Shutdown(
                admin_pb2.ShutdownRequest(), metadata=self._metadata
            )
        )

    @staticmethod
    def _unary(call):
        """Run a unary RPC, mapping a gRPC status error to a PawrlyError."""
        try:
            return call()
        except grpc.RpcError as e:
            raise _status_to_error(e) from None

    def close(self) -> None:
        self._channel.close()


def _grpc_rows(it, pending, meta: dict) -> Iterator[dict]:
    def process(frame) -> Iterator[dict]:
        which = frame.WhichOneof("payload")
        if which == "ipc_stream":
            table = pa.ipc.open_stream(frame.ipc_stream).read_all()
            if not meta["columns"]:
                meta["columns"] = table.column_names
            for row in table.to_pylist():
                meta["row_count"] += 1
                yield row
        elif which == "completed":
            meta["row_count"] = frame.completed.rows_returned
            meta["truncated"] = frame.completed.truncated
        elif which == "error":
            raise PawrlyError(frame.error.code, frame.error.message)

    try:
        if pending is not None:
            yield from process(pending)
        for frame in it:
            yield from process(frame)
    except grpc.RpcError as e:
        raise _status_to_error(e) from None


def _status_to_error(err: grpc.RpcError) -> PawrlyError:
    """Map a gRPC status error to a PawrlyError, reading the stable `PAWRLY_*`
    code the server puts in trailing metadata (`pawrly-error-code`)."""
    code = "PAWRLY_INTERNAL"
    for key, value in err.trailing_metadata() or ():
        if key == "pawrly-error-code":
            code = value.decode() if isinstance(value, (bytes, bytearray)) else value
            break
    return PawrlyError(code, err.details() or "")


def _require_namespace_echo(requested: str | None, echoed: str | None) -> None:
    if requested and requested != echoed:
        raise PawrlyError(
            "PAWRLY_PROTOCOL",
            f"server ignored namespace `{requested}` — it predates materialize "
            "namespaces, so the operation targeted the default namespace instead; "
            "upgrade the server",
        )


def _denum(enum_cls, value: int, prefix: str) -> str:
    """Proto enum int → the engine's lowercase string (SOURCE_KIND_FILE → file)."""
    return enum_cls.Name(value).removeprefix(prefix).lower()


def _table_name_pb(name: str) -> common_pb2.TableName:
    schema, _, table = name.partition(".")
    return common_pb2.TableName(schema=schema, table=table)


def _source_info(s) -> SourceInfo:
    return SourceInfo(
        name=s.name,
        kind=_denum(common_pb2.SourceKind, s.kind, "SOURCE_KIND_"),
        status=_denum(common_pb2.SourceStatus, s.status, "SOURCE_STATUS_"),
        status_detail=s.status_detail or None,
        sub_kind=s.sub_kind if s.HasField("sub_kind") else None,
        table_count=s.table_count,
        registered_at=s.registered_at.ToJsonString()
        if s.HasField("registered_at")
        else "",
    )


def _table_info(t) -> TableInfo:
    return TableInfo(
        name=TableName(schema=t.name.schema, table=t.name.table),
        kind=_denum(common_pb2.SourceKind, t.kind, "SOURCE_KIND_"),
        description=t.description or None,
        row_count_estimate=t.row_count_estimate
        if t.HasField("row_count_estimate")
        else None,
        cached=t.cached,
        required_filters=list(t.required_filters),
    )


def _column_spec(c) -> ColumnSpec:
    return ColumnSpec(
        name=c.name,
        data_type=c.data_type,
        nullable=c.nullable,
        description=c.description or None,
        is_filter_pushable=c.is_filter_pushable,
        is_required_filter=c.is_required_filter,
    )


def _cache_entry(e) -> CacheEntryInfo:
    return CacheEntryInfo(
        name=TableName(schema=e.name.schema, table=e.name.table),
        mode=_denum(common_pb2.CacheMode, e.mode, "CACHE_MODE_"),
        written_at=e.written_at.ToJsonString() if e.HasField("written_at") else "",
        expires_at=e.expires_at.ToJsonString() if e.HasField("expires_at") else None,
        row_count=e.row_count,
        size_bytes=e.size_bytes,
        file_count=e.file_count,
    )


def _function_info(f) -> FunctionInfo:
    return FunctionInfo(
        namespace=f.namespace,
        name=f.name,
        kind=f.kind,
        builtin=f.builtin,
        signature=f.signature,
        description=f.description if f.HasField("description") else None,
    )


def _model_description(m) -> SemanticModelDescription:
    return SemanticModelDescription(
        name=m.name,
        description=m.description or None,
        source=m.source,
        primary_key=list(m.primary_key),
        dimensions=[
            SemanticDimension(
                name=d.name,
                expr=d.expr,
                type=_denum(semantic_pb2.DimensionType, d.type, "DIMENSION_TYPE_"),
                grains=[
                    _denum(semantic_pb2.TimeGrain, g, "TIME_GRAIN_") for g in d.grains
                ],
                description=d.description or None,
            )
            for d in m.dimensions
        ],
        measures=[
            SemanticMeasure(
                name=x.name,
                agg=x.agg,
                custom_sql=x.custom_sql or None,
                expr=x.expr,
                filters=list(x.filters),
                format=x.format or None,
                description=x.description or None,
            )
            for x in m.measures
        ],
        relationships=[
            SemanticRelationship(
                name=r.name,
                kind=_denum(semantic_pb2.RelationshipKind, r.kind, "RELATIONSHIP_KIND_"),
                target=r.target,
                on=r.on,
            )
            for r in m.relationships
        ],
        segments=[
            SemanticSegment(
                name=s.name,
                description=s.description or None,
                filters=[
                    SemanticFilter(
                        member=f.member,
                        op=_denum(semantic_pb2.FilterOp, f.op, "FILTER_OP_"),
                        values=list(f.values),
                    )
                    for f in s.filters
                ],
            )
            for s in m.segments
        ],
    )


def _function_description(f) -> FunctionDescription:
    return FunctionDescription(
        namespace=f.namespace,
        name=f.name,
        kind=f.kind,
        builtin=f.builtin,
        signature=f.signature,
        description=f.description if f.HasField("description") else None,
        wiki=f.wiki if f.HasField("wiki") else None,
        examples=list(f.examples),
        args=[
            FunctionArg(
                name=a.name,
                type=a.type,
                required=a.required,
                default=a.default if a.HasField("default") else None,
                description=a.description if a.HasField("description") else None,
                tool_arg=a.tool_arg if a.HasField("tool_arg") else None,
            )
            for a in f.args
        ],
        returns=[
            FunctionColumn(
                name=c.name,
                type=c.type,
                source=c.source if c.HasField("source") else None,
                description=c.description if c.HasField("description") else None,
            )
            for c in f.returns
        ],
    )
