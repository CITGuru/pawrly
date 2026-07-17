from .query import QueryHandle
from .result import (
    CacheEntryInfo,
    CatalogSnapshot,
    FunctionDescription,
    FunctionInfo,
    HealthReport,
    MaterializeOutcome,
    MaterializeSpec,
    RefreshCatalogOutcome,
    RefreshOutcome,
    ReloadReport,
    SemanticModelDescription,
    SemanticModelInfo,
    SemanticQuery,
    SourceInfo,
    SourceTestReport,
    TableDescription,
    TableInfo,
    VacuumReport,
)


class PawrlyClient:
    """One `EngineService` surface; every method is identical regardless of wire."""

    def __init__(self, transport) -> None:
        self._t = transport

    @classmethod
    def rest(cls, base_url: str, bearer: str | None = None) -> "PawrlyClient":
        from .transports.rest import RestTransport

        return cls(RestTransport(base_url, bearer))

    @classmethod
    def grpc(cls, endpoint: str, bearer: str | None = None) -> "PawrlyClient":
        # Lazy: the gRPC transport needs the `grpc` extra + generated stubs.
        from .transports.grpc import GrpcTransport

        return cls(GrpcTransport(endpoint, bearer))

    @classmethod
    def local(
        cls,
        config: str | None = None,
        home: str | None = None,
        binary: str = "pawrly",
    ) -> "PawrlyClient":
        """Run the engine in a `pawrly console` child this client owns; `close()`
        (or a ``with`` block) stops it."""
        from .transports.local import LocalTransport

        return cls(LocalTransport(config, home, binary))

    def __enter__(self) -> "PawrlyClient":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    @property
    def transport(self) -> str:
        return self._t.name

    def query(
        self,
        sql: str,
        params: dict[str, str] | None = None,
        limit: int | None = None,
    ) -> QueryHandle:
        return self._t.query(sql, params or {}, limit)

    def semantic_query(self, q: SemanticQuery) -> QueryHandle:
        return self._t.semantic_query(q)

    def explain(self, sql: str, analyze: bool = False) -> str:
        return self._t.explain(sql, analyze)

    def cancel(self, query_id: str) -> bool:
        return self._t.cancel(query_id)

    def materialize(
        self, name: str, spec: MaterializeSpec, namespace: str | None = None
    ) -> MaterializeOutcome:
        return self._t.materialize(name, spec, namespace)

    def list_sources(self) -> list[SourceInfo]:
        return self._t.list_sources()

    def list_tables(
        self, source: str | None = None, name_glob: str | None = None
    ) -> list[TableInfo]:
        return self._t.list_tables(source, name_glob)

    def describe_table(self, name: str) -> TableDescription:
        return self._t.describe_table(name)

    def schema_snapshot(
        self, sources: list[str] | None = None, compact: bool = False
    ) -> CatalogSnapshot:
        return self._t.schema_snapshot(sources, compact)

    def cache_entries(self, namespace: str | None = None) -> list[CacheEntryInfo]:
        return self._t.cache_entries(namespace)

    def list_functions(self) -> list[FunctionInfo]:
        return self._t.list_functions()

    def describe_function(self, namespace: str, name: str) -> FunctionDescription:
        return self._t.describe_function(namespace, name)

    def list_semantic_models(self) -> list[SemanticModelInfo]:
        return self._t.list_semantic_models()

    def describe_semantic_model(self, name: str) -> SemanticModelDescription:
        return self._t.describe_semantic_model(name)

    def add_source(self, definition: dict) -> SourceInfo:
        return self._t.add_source(definition)

    def remove_source(self, name: str) -> bool:
        return self._t.remove_source(name)

    def test_source(self, name: str) -> SourceTestReport:
        return self._t.test_source(name)

    def reload_config(self) -> ReloadReport:
        return self._t.reload_config()

    def refresh_catalog(self, source: str | None = None) -> RefreshCatalogOutcome:
        return self._t.refresh_catalog(source)

    def refresh_table(
        self, name: str, namespace: str | None = None
    ) -> RefreshOutcome:
        return self._t.refresh_table(name, namespace)

    def invalidate_cache(self, name: str) -> bool:
        return self._t.invalidate_cache(name)

    def vacuum_cache(self) -> VacuumReport:
        return self._t.vacuum_cache()

    def drop_materialized(self, name: str, namespace: str | None = None) -> bool:
        return self._t.drop_materialized(name, namespace)

    def health(self) -> HealthReport:
        return self._t.health()

    def shutdown(self) -> None:
        return self._t.shutdown()

    def close(self) -> None:
        self._t.close()
