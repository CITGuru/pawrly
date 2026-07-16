import { PawrlyError, UnsupportedByTransport } from "../errors.js";
import { QueryHandle, type QueryMeta, type Row } from "../query.js";
import type {
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
  SourceDef,
  SourceInfo,
  SourceTestReport,
  TableDescription,
  TableInfo,
  VacuumReport,
} from "../result.js";
import type { Transport } from "../transport.js";
import * as convert from "./convert.js";

export class RestTransport implements Transport {
  readonly name = "rest";
  private readonly base: string;

  constructor(
    baseUrl: string,
    private readonly bearer?: string,
  ) {
    this.base = baseUrl.replace(/\/+$/, "");
  }

  private headers(): Headers {
    const h = new Headers();
    h.set("content-type", "application/json");
    if (this.bearer) h.set("authorization", `Bearer ${this.bearer}`);
    return h;
  }

  private async send(path: string, init?: RequestInit): Promise<Record<string, unknown>> {
    const resp = await fetch(`${this.base}${path}`, { ...init, headers: this.headers() });
    const body = (await resp.json().catch(() => ({}))) as Record<string, unknown>;
    if (!resp.ok) {
      const err = (body.error ?? {}) as { code?: string; message?: string };
      throw new PawrlyError(err.code ?? "PAWRLY_INTERNAL", err.message ?? resp.statusText);
    }
    return body;
  }

  async query(
    sql: string,
    params: Record<string, string>,
    limit?: number,
  ): Promise<QueryHandle> {
    // Stream NDJSON so large results stay memory-bounded. NDJSON carries no id
    // or completion envelope, so `id` is empty and `truncated` stays false.
    const payload: Record<string, unknown> = { sql, params, format: "ndjson" };
    if (limit !== undefined) payload.limit = limit;
    const resp = await fetch(`${this.base}/v1/sql`, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify(payload),
    });
    if (!resp.ok) {
      const body = (await resp.json().catch(() => ({}))) as Record<string, unknown>;
      const err = (body.error ?? {}) as { code?: string; message?: string };
      throw new PawrlyError(err.code ?? "PAWRLY_INTERNAL", err.message ?? resp.statusText);
    }
    const meta: QueryMeta = { columns: [], rowCount: 0, truncated: false };
    return new QueryHandle("", ndjsonRows(resp.body, meta), meta);
  }

  async semanticQuery(q: SemanticQuery): Promise<QueryHandle> {
    // `/v1/query` returns a buffered JSON envelope (no NDJSON there yet).
    const r = await this.send("/v1/query", {
      method: "POST",
      body: JSON.stringify(semanticToJson(q)),
    });
    return bufferedHandle(
      "",
      (r.columns as string[]) ?? [],
      (r.rows as Row[]) ?? [],
      (r.truncated as boolean) ?? false,
    );
  }

  async explain(sql: string, analyze: boolean): Promise<string> {
    const r = await this.send("/v1/explain", {
      method: "POST",
      body: JSON.stringify({ sql, analyze }),
    });
    return (r.plan as string) ?? "";
  }

  async cancel(queryId: string): Promise<boolean> {
    const r = await this.send(`/v1/queries/${encodeURIComponent(queryId)}/cancel`, {
      method: "POST",
    });
    return (r.cancelled as boolean) ?? false;
  }

  async materialize(
    name: string,
    spec: MaterializeSpec,
    namespace?: string,
  ): Promise<MaterializeOutcome> {
    const r = await this.send(`/v1/materialized/${encodeURIComponent(name)}${nsQuery(namespace)}`, {
      method: "PUT",
      body: JSON.stringify(spec),
    });
    requireNamespaceEcho(namespace, r.namespace as string | undefined);
    const n = (r.name ?? {}) as { schema?: string; table?: string };
    return {
      name: { schema: n.schema ?? "", table: n.table ?? "" },
      filePath: (r.file_path as string) ?? "",
      rowCount: (r.row_count as number) ?? 0,
      sizeBytes: (r.size_bytes as number) ?? 0,
    };
  }

  async listSources(): Promise<SourceInfo[]> {
    const r = await this.send("/v1/sources");
    return ((r.sources as Record<string, unknown>[]) ?? []).map(convert.sourceInfo);
  }

  async listTables(source?: string, nameGlob?: string): Promise<TableInfo[]> {
    const q = new URLSearchParams();
    if (source !== undefined) q.set("source", source);
    if (nameGlob !== undefined) q.set("name_glob", nameGlob);
    const r = await this.send(`/v1/tables${qs(q)}`);
    return ((r.tables as Record<string, unknown>[]) ?? []).map(convert.tableInfo);
  }

  async describeTable(name: string): Promise<TableDescription> {
    return convert.tableDescription(await this.send(`/v1/tables/${encodeURIComponent(name)}`));
  }

  async schemaSnapshot(sources?: string[], compact?: boolean): Promise<CatalogSnapshot> {
    const q = new URLSearchParams();
    if (sources?.length) q.set("sources", sources.join(","));
    if (compact) q.set("compact", "true");
    return convert.catalogSnapshot(await this.send(`/v1/schema${qs(q)}`));
  }

  async cacheEntries(namespace?: string): Promise<CacheEntryInfo[]> {
    const r = await this.send(`/v1/cache${nsQuery(namespace)}`);
    requireNamespaceEcho(namespace, r.namespace as string | undefined);
    return ((r.entries as Record<string, unknown>[]) ?? []).map(convert.cacheEntry);
  }

  async listFunctions(): Promise<FunctionInfo[]> {
    const r = await this.send("/v1/functions");
    return ((r.functions as Record<string, unknown>[]) ?? []).map(convert.functionInfo);
  }

  async describeFunction(namespace: string, name: string): Promise<FunctionDescription> {
    const path = `/v1/functions/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}`;
    return convert.functionDescription(await this.send(path));
  }

  async listSemanticModels(): Promise<SemanticModelInfo[]> {
    const r = await this.send("/v1/semantic/models");
    return ((r.models as Record<string, unknown>[]) ?? []).map(convert.semanticModelInfo);
  }

  async describeSemanticModel(name: string): Promise<SemanticModelDescription> {
    return convert.semanticModelDescription(
      await this.send(`/v1/semantic/models/${encodeURIComponent(name)}`),
    );
  }

  async addSource(def: SourceDef): Promise<SourceInfo> {
    return convert.sourceInfo(
      await this.send("/v1/sources", { method: "POST", body: JSON.stringify(def) }),
    );
  }

  async removeSource(name: string): Promise<boolean> {
    const r = await this.send(`/v1/sources/${encodeURIComponent(name)}`, { method: "DELETE" });
    return (r.removed as boolean) ?? false;
  }

  async testSource(name: string): Promise<SourceTestReport> {
    const path = `/v1/sources/${encodeURIComponent(name)}/test`;
    return convert.sourceTestReport(await this.send(path, { method: "POST" }));
  }

  async reloadConfig(): Promise<ReloadReport> {
    return convert.reloadReport(await this.send("/v1/config/reload", { method: "POST" }));
  }

  async refreshCatalog(source?: string): Promise<RefreshCatalogOutcome> {
    const q = new URLSearchParams();
    if (source !== undefined) q.set("source", source);
    return convert.refreshCatalogOutcome(
      await this.send(`/v1/catalog/refresh${qs(q)}`, { method: "POST" }),
    );
  }

  async refreshTable(name: string): Promise<RefreshOutcome> {
    const path = `/v1/tables/${encodeURIComponent(name)}/refresh`;
    return convert.refreshOutcome(await this.send(path, { method: "POST" }));
  }

  async invalidateCache(name: string): Promise<boolean> {
    const r = await this.send(`/v1/cache/${encodeURIComponent(name)}`, { method: "DELETE" });
    return (r.invalidated as boolean) ?? false;
  }

  async vacuumCache(): Promise<VacuumReport> {
    return convert.vacuumReport(await this.send("/v1/cache/vacuum", { method: "POST" }));
  }

  async dropMaterialized(name: string, namespace?: string): Promise<boolean> {
    const r = await this.send(`/v1/materialized/${encodeURIComponent(name)}${nsQuery(namespace)}`, {
      method: "DELETE",
    });
    requireNamespaceEcho(namespace, r.namespace as string | undefined);
    return (r.dropped as boolean) ?? false;
  }

  async health(): Promise<HealthReport> {
    const r = await this.send("/v1/health");
    return { ok: (r.ok as boolean) ?? false, version: (r.version as string) ?? "" };
  }

  async shutdown(): Promise<void> {
    throw new UnsupportedByTransport("shutdown", "rest");
  }

  close(): void {}
}

function qs(params: URLSearchParams): string {
  const s = params.toString();
  return s ? `?${s}` : "";
}

function nsQuery(namespace?: string): string {
  return namespace ? `?namespace=${encodeURIComponent(namespace)}` : "";
}

function requireNamespaceEcho(requested: string | undefined, echoed: string | undefined): void {
  if (requested && requested !== echoed) {
    throw new PawrlyError(
      "PAWRLY_PROTOCOL",
      `server ignored namespace \`${requested}\` — it predates materialize namespaces, so the ` +
        "operation targeted the default namespace instead; upgrade the server",
    );
  }
}

async function* ndjsonRows(
  body: ReadableStream<Uint8Array> | null,
  meta: QueryMeta,
): AsyncGenerator<Row> {
  if (!body) return;
  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of body as unknown as AsyncIterable<Uint8Array>) {
    buf += decoder.decode(chunk, { stream: true });
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (line) yield emit(line, meta);
    }
  }
  const tail = (buf + decoder.decode()).trim();
  if (tail) yield emit(tail, meta);
}

function emit(line: string, meta: QueryMeta): Row {
  const row = JSON.parse(line) as Row;
  if (meta.columns.length === 0) meta.columns = Object.keys(row);
  meta.rowCount += 1;
  return row;
}

function bufferedHandle(
  id: string,
  columns: string[],
  rows: Row[],
  truncated: boolean,
): QueryHandle {
  const meta: QueryMeta = { columns, rowCount: rows.length, truncated };
  async function* gen(): AsyncGenerator<Row> {
    for (const r of rows) yield r;
  }
  return new QueryHandle(id, gen(), meta);
}

function semanticToJson(q: SemanticQuery): Record<string, unknown> {
  const body: Record<string, unknown> = {
    measures: q.measures ?? [],
    dimensions: q.dimensions ?? [],
    params: q.params ?? {},
  };
  if (q.filters) {
    body.filters = q.filters.map((f) => ({
      member: f.member,
      op: f.op,
      values: f.values ?? [],
    }));
  }
  if (q.orderBy) {
    body.order_by = q.orderBy.map((o) => ({
      member: o.member,
      direction: o.desc ? "desc" : "asc",
    }));
  }
  if (q.segments) body.segments = q.segments;
  if (q.limit !== undefined) body.limit = q.limit;
  if (q.timeZone) body.time_zone = q.timeZone;
  return body;
}
