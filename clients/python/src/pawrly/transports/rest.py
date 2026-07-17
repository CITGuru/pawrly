"""``EngineService`` over the JSON REST surface (``pawrly console``)."""

import json
from collections.abc import Iterator

import requests

from ..errors import PawrlyError, UnsupportedByTransport
from ..query import QueryHandle
from ..result import (
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
from . import convert


class RestTransport:
    name = "rest"

    def __init__(self, base_url: str, bearer: str | None = None) -> None:
        self._base = base_url.rstrip("/")
        self._session = requests.Session()
        if bearer:
            self._session.headers["authorization"] = f"Bearer {bearer}"

    def _send(
        self,
        method: str,
        path: str,
        json_body: dict | None = None,
        params: dict | None = None,
    ) -> dict:
        resp = self._session.request(
            method, self._base + path, json=json_body, params=params
        )
        body = resp.json() if resp.content else {}
        if not resp.ok:
            err = body.get("error", {}) if isinstance(body, dict) else {}
            raise PawrlyError(
                err.get("code", "PAWRLY_INTERNAL"),
                err.get("message", resp.reason or "request failed"),
            )
        return body

    def query(
        self, sql: str, params: dict[str, str], limit: int | None = None
    ) -> QueryHandle:
        # Stream NDJSON so large results stay memory-bounded. NDJSON carries no id
        # or completion envelope, so `id` is empty and `truncated` stays False.
        payload: dict = {"sql": sql, "params": params, "format": "ndjson"}
        if limit is not None:
            payload["limit"] = limit
        resp = self._session.post(self._base + "/v1/sql", json=payload, stream=True)
        if not resp.ok:
            body = resp.json() if resp.content else {}
            err = body.get("error", {}) if isinstance(body, dict) else {}
            raise PawrlyError(
                err.get("code", "PAWRLY_INTERNAL"),
                err.get("message", resp.reason or "request failed"),
            )
        meta = {"columns": [], "row_count": 0, "truncated": False}
        return QueryHandle("", _ndjson_rows(resp, meta), meta)

    def semantic_query(self, q: SemanticQuery) -> QueryHandle:
        # `/v1/query` returns a buffered JSON envelope (no NDJSON there yet).
        body: dict = {
            "measures": q.measures or [],
            "dimensions": q.dimensions or [],
            "params": q.params or {},
        }
        if q.filters:
            body["filters"] = [
                {"member": f.member, "op": f.op, "values": f.values or []}
                for f in q.filters
            ]
        if q.order_by:
            body["order_by"] = [
                {"member": o.member, "direction": "desc" if o.desc else "asc"}
                for o in q.order_by
            ]
        if q.segments:
            body["segments"] = q.segments
        if q.limit is not None:
            body["limit"] = q.limit
        if q.time_zone:
            body["time_zone"] = q.time_zone
        r = self._send("POST", "/v1/query", body)
        rows = r.get("rows", [])
        meta = {
            "columns": r.get("columns", []),
            "row_count": r.get("row_count", len(rows)),
            "truncated": r.get("truncated", False),
        }
        return QueryHandle("", iter(rows), meta)

    def explain(self, sql: str, analyze: bool) -> str:
        return self._send("POST", "/v1/explain", {"sql": sql, "analyze": analyze}).get(
            "plan", ""
        )

    def cancel(self, query_id: str) -> bool:
        return self._send("POST", f"/v1/queries/{query_id}/cancel").get(
            "cancelled", False
        )

    def materialize(
        self, name: str, spec: MaterializeSpec, namespace: str | None = None
    ) -> MaterializeOutcome:
        body = {"kind": spec.kind, "sql": spec.sql, "params": spec.params or {}}
        r = self._send("PUT", f"/v1/materialized/{name}{_ns_query(namespace)}", body)
        _require_namespace_echo(namespace, r.get("namespace"))
        n = r.get("name", {})
        return MaterializeOutcome(
            name={"schema": n.get("schema", ""), "table": n.get("table", "")},
            file_path=r.get("file_path", ""),
            row_count=r.get("row_count", 0),
            size_bytes=r.get("size_bytes", 0),
        )

    def list_sources(self) -> list[SourceInfo]:
        r = self._send("GET", "/v1/sources")
        return [convert.source_info(s) for s in r.get("sources", [])]

    def list_tables(
        self, source: str | None = None, name_glob: str | None = None
    ) -> list[TableInfo]:
        params = {}
        if source is not None:
            params["source"] = source
        if name_glob is not None:
            params["name_glob"] = name_glob
        r = self._send("GET", "/v1/tables", params=params)
        return [convert.table_info(t) for t in r.get("tables", [])]

    def describe_table(self, name: str) -> TableDescription:
        return convert.table_description(self._send("GET", f"/v1/tables/{name}"))

    def schema_snapshot(
        self, sources: list[str] | None = None, compact: bool = False
    ) -> CatalogSnapshot:
        params = {}
        if sources:
            params["sources"] = ",".join(sources)
        if compact:
            params["compact"] = "true"
        return convert.catalog_snapshot(self._send("GET", "/v1/schema", params=params))

    def cache_entries(self, namespace: str | None = None) -> list[CacheEntryInfo]:
        r = self._send("GET", f"/v1/cache{_ns_query(namespace)}")
        _require_namespace_echo(namespace, r.get("namespace"))
        return [convert.cache_entry(e) for e in r.get("entries", [])]

    def list_functions(self) -> list[FunctionInfo]:
        r = self._send("GET", "/v1/functions")
        return [convert.function_info(f) for f in r.get("functions", [])]

    def describe_function(self, namespace: str, name: str) -> FunctionDescription:
        return convert.function_description(
            self._send("GET", f"/v1/functions/{namespace}/{name}")
        )

    def list_semantic_models(self) -> list[SemanticModelInfo]:
        r = self._send("GET", "/v1/semantic/models")
        return [convert.semantic_model_info(m) for m in r.get("models", [])]

    def describe_semantic_model(self, name: str) -> SemanticModelDescription:
        return convert.semantic_model_description(
            self._send("GET", f"/v1/semantic/models/{name}")
        )

    def list_metrics(self) -> list[dict]:
        return self._send("GET", "/v1/semantic/metrics").get("metrics", [])

    def describe_metric(self, name: str) -> dict:
        return self._send("GET", f"/v1/semantic/metrics/{name}")

    def add_source(self, definition: dict) -> SourceInfo:
        return convert.source_info(self._send("POST", "/v1/sources", definition))

    def remove_source(self, name: str) -> bool:
        return self._send("DELETE", f"/v1/sources/{name}").get("removed", False)

    def test_source(self, name: str) -> SourceTestReport:
        return convert.source_test_report(self._send("POST", f"/v1/sources/{name}/test"))

    def reload_config(self) -> ReloadReport:
        return convert.reload_report(self._send("POST", "/v1/config/reload"))

    def refresh_catalog(self, source: str | None = None) -> RefreshCatalogOutcome:
        params = {"source": source} if source is not None else None
        return convert.refresh_catalog_outcome(
            self._send("POST", "/v1/catalog/refresh", params=params)
        )

    def refresh_table(
        self, name: str, namespace: str | None = None
    ) -> RefreshOutcome:
        r = self._send("POST", f"/v1/tables/{name}/refresh{_ns_query(namespace)}")
        _require_namespace_echo(namespace, r.get("namespace"))
        return convert.refresh_outcome(r)

    def invalidate_cache(self, name: str) -> bool:
        return self._send("DELETE", f"/v1/cache/{name}").get("invalidated", False)

    def vacuum_cache(self) -> VacuumReport:
        return convert.vacuum_report(self._send("POST", "/v1/cache/vacuum"))

    def drop_materialized(self, name: str, namespace: str | None = None) -> bool:
        r = self._send("DELETE", f"/v1/materialized/{name}{_ns_query(namespace)}")
        _require_namespace_echo(namespace, r.get("namespace"))
        return r.get("dropped", False)

    def health(self) -> HealthReport:
        r = self._send("GET", "/v1/health")
        return HealthReport(ok=r.get("ok", False), version=r.get("version", ""))

    def shutdown(self) -> None:
        raise UnsupportedByTransport("shutdown", "rest")

    def close(self) -> None:
        self._session.close()


def _require_namespace_echo(requested: str | None, echoed: str | None) -> None:
    if requested and requested != echoed:
        raise PawrlyError(
            "PAWRLY_PROTOCOL",
            f"server ignored namespace `{requested}` — it predates materialize "
            "namespaces, so the operation targeted the default namespace instead; "
            "upgrade the server",
        )


def _ns_query(namespace: str | None) -> str:
    if not namespace:
        return ""
    return f"?namespace={requests.utils.quote(namespace, safe='')}"


def _ndjson_rows(resp, meta: dict) -> Iterator[dict]:
    try:
        for line in resp.iter_lines():
            if not line:
                continue
            row = json.loads(line)
            if not meta["columns"]:
                meta["columns"] = list(row.keys())
            meta["row_count"] += 1
            yield row
    finally:
        resp.close()
