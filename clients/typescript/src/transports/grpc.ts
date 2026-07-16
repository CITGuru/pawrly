import {
  ConnectError,
  createClient,
  type Client,
  type Interceptor,
} from "@connectrpc/connect";
import { createGrpcTransport } from "@connectrpc/connect-node";
import { tableFromIPC } from "apache-arrow";
import { timestampDate } from "@bufbuild/protobuf/wkt";

import { PawrlyError } from "../errors.js";
import { QueryHandle, type QueryMeta, type Row } from "../query.js";
import type {
  CacheEntryInfo,
  CatalogSnapshot,
  ColumnSpec,
  FilterOp,
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
  TableName,
  VacuumReport,
} from "../result.js";
import type { Transport } from "../transport.js";
import * as convert from "./convert.js";
import { AdminService } from "../gen/pawrly/v1/admin_pb.js";
import {
  CacheService,
  type CacheEntryInfo as PbCacheEntryInfo,
} from "../gen/pawrly/v1/cache_pb.js";
import { CatalogService } from "../gen/pawrly/v1/catalog_pb.js";
import {
  CacheMode,
  SourceKind,
  SourceStatus,
  type ColumnSpec as PbColumnSpec,
  type SourceInfo as PbSourceInfo,
  type TableInfo as PbTableInfo,
} from "../gen/pawrly/v1/common_pb.js";
import { QueryService, type QueryResponse } from "../gen/pawrly/v1/query_pb.js";
import { SourcesService } from "../gen/pawrly/v1/sources_pb.js";
import {
  SemanticService,
  FilterOp as PbFilterOp,
  DimensionType,
  TimeGrain,
  RelationshipKind,
  type ModelDescription as PbModelDescription,
} from "../gen/pawrly/v1/semantic_pb.js";
import type {
  FunctionDescription as PbFunctionDescription,
  FunctionInfo as PbFunctionInfo,
} from "../gen/pawrly/v1/catalog_pb.js";

export class GrpcTransport implements Transport {
  readonly name = "grpc";
  private readonly query_: Client<typeof QueryService>;
  private readonly semantic_: Client<typeof SemanticService>;
  private readonly cache_: Client<typeof CacheService>;
  private readonly admin_: Client<typeof AdminService>;
  private readonly catalog_: Client<typeof CatalogService>;
  private readonly sources_: Client<typeof SourcesService>;

  constructor(endpoint: string, bearer?: string) {
    const auth: Interceptor = (next) => (req) => {
      if (bearer) req.header.set("Authorization", `Bearer ${bearer}`);
      return next(req);
    };
    const transport = createGrpcTransport({
      baseUrl: toHttpUrl(endpoint),
      interceptors: [auth],
    });
    this.query_ = createClient(QueryService, transport);
    this.semantic_ = createClient(SemanticService, transport);
    this.cache_ = createClient(CacheService, transport);
    this.admin_ = createClient(AdminService, transport);
    this.catalog_ = createClient(CatalogService, transport);
    this.sources_ = createClient(SourcesService, transport);
  }

  async query(
    sql: string,
    params: Record<string, string>,
    limit?: number,
  ): Promise<QueryHandle> {
    return streamFrames(this.query_.query({ sql, params, maxRows: BigInt(limit ?? 0) }));
  }

  async semanticQuery(q: SemanticQuery): Promise<QueryHandle> {
    return streamFrames(this.semantic_.semanticQuery(semanticToProto(q)));
  }

  async explain(sql: string, analyze: boolean): Promise<string> {
    return (await unary(this.query_.explain({ sql, analyze }))).plan;
  }

  async cancel(queryId: string): Promise<boolean> {
    return (await unary(this.query_.cancel({ queryId }))).cancelled;
  }

  async materialize(
    name: string,
    spec: MaterializeSpec,
    namespace?: string,
  ): Promise<MaterializeOutcome> {
    const resp = await unary(
      this.cache_.materialize({
        name,
        namespace: namespace ?? "",
        spec: {
          spec: { case: "query", value: { sql: spec.sql, params: spec.params ?? {} } },
        },
      }),
    );
    requireNamespaceEcho(namespace, resp.namespace);
    return {
      name: { schema: resp.name?.schema ?? "", table: resp.name?.table ?? "" },
      filePath: resp.filePath,
      rowCount: Number(resp.rowCount),
      sizeBytes: Number(resp.sizeBytes),
    };
  }

  async listSources(): Promise<SourceInfo[]> {
    const resp = await unary(this.sources_.listSources({}));
    return resp.sources.map(pbSourceInfo);
  }

  async listTables(source?: string, nameGlob?: string): Promise<TableInfo[]> {
    const resp = await unary(this.catalog_.listTables({ source, nameGlob }));
    return resp.tables.map(pbTableInfo);
  }

  async describeTable(name: string): Promise<TableDescription> {
    const resp = await unary(this.catalog_.describeTable({ name: pbTableName(name) }));
    return {
      table: resp.table ? pbTableInfo(resp.table) : emptyTableInfo(),
      columns: resp.columns.map(pbColumnSpec),
      pushableFilterColumns: resp.pushableFilterColumns,
      examples: resp.examples,
      wiki: resp.wiki,
    };
  }

  async schemaSnapshot(sources?: string[], compact?: boolean): Promise<CatalogSnapshot> {
    const resp = await unary(
      this.catalog_.schemaSnapshot({ sources: sources ?? [], compact: compact ?? false }),
    );
    // The snapshot travels as serde JSON bytes — the same shape REST returns.
    return convert.catalogSnapshot(JSON.parse(new TextDecoder().decode(resp.snapshotJson)));
  }

  async cacheEntries(namespace?: string): Promise<CacheEntryInfo[]> {
    const resp = await unary(this.cache_.listEntries({ namespace: namespace ?? "" }));
    requireNamespaceEcho(namespace, resp.namespace);
    return resp.entries.map(pbCacheEntry);
  }

  async listFunctions(): Promise<FunctionInfo[]> {
    const resp = await unary(this.catalog_.listFunctions({}));
    return resp.functions.map(pbFunctionInfo);
  }

  async describeFunction(namespace: string, name: string): Promise<FunctionDescription> {
    const resp = await unary(this.catalog_.describeFunction({ namespace, name }));
    return resp.function ? pbFunctionDescription(resp.function) : emptyFunctionDescription();
  }

  async listSemanticModels(): Promise<SemanticModelInfo[]> {
    const resp = await unary(this.semantic_.listModels({}));
    return resp.models.map((m) => ({
      name: m.name,
      description: m.description || undefined,
      source: m.source,
      dimensionCount: m.dimensionCount,
      measureCount: m.measureCount,
    }));
  }

  async describeSemanticModel(name: string): Promise<SemanticModelDescription> {
    const resp = await unary(this.semantic_.describeModel({ name }));
    return resp.model ? pbModelDescription(resp.model) : emptyModelDescription();
  }

  async addSource(def: SourceDef): Promise<SourceInfo> {
    // gRPC AddSource takes YAML; JSON is valid YAML, so serialize the object.
    const resp = await unary(this.sources_.addSource({ yaml: JSON.stringify(def) }));
    return resp.source ? pbSourceInfo(resp.source) : emptySourceInfo();
  }

  async removeSource(name: string): Promise<boolean> {
    return (await unary(this.sources_.removeSource({ name }))).removed;
  }

  async testSource(name: string): Promise<SourceTestReport> {
    const resp = await unary(this.sources_.testSource({ name }));
    return { name, ok: resp.ok, latencyMs: durMs(resp.latency), detail: resp.detail || undefined };
  }

  async reloadConfig(): Promise<ReloadReport> {
    const resp = await unary(this.sources_.reloadConfig({}));
    return {
      sourcesAdded: Number(resp.sourcesAdded),
      sourcesRemoved: Number(resp.sourcesRemoved),
      sourcesChanged: Number(resp.sourcesChanged),
    };
  }

  async refreshCatalog(source?: string): Promise<RefreshCatalogOutcome> {
    const resp = await unary(this.catalog_.refreshCatalog({ source }));
    return {
      sourcesRefreshed: Number(resp.sourcesRefreshed),
      tablesDiscovered: Number(resp.tablesDiscovered),
    };
  }

  async refreshTable(name: string): Promise<RefreshOutcome> {
    const resp = await unary(this.cache_.refresh({ name: pbTableName(name) }));
    return {
      table: pbTableName(name),
      rowsWritten: Number(resp.rowsWritten),
      sizeBytes: Number(resp.sizeBytes),
      elapsedMs: durMs(resp.elapsed),
      expiresAt: isoTs(resp.expiresAt),
    };
  }

  async invalidateCache(name: string): Promise<boolean> {
    return (await unary(this.cache_.invalidate({ name: pbTableName(name) }))).removed;
  }

  async vacuumCache(): Promise<VacuumReport> {
    const resp = await unary(this.cache_.vacuum({}));
    return {
      entriesRemoved: Number(resp.entriesRemoved),
      filesRemoved: Number(resp.filesRemoved),
      bytesReclaimed: Number(resp.bytesReclaimed),
    };
  }

  async dropMaterialized(name: string, namespace?: string): Promise<boolean> {
    const resp = await unary(this.cache_.dropMaterialized({ name, namespace: namespace ?? "" }));
    requireNamespaceEcho(namespace, resp.namespace);
    return resp.dropped;
  }

  async health(): Promise<HealthReport> {
    const resp = await unary(this.admin_.health({}));
    return { ok: resp.ok, version: resp.version };
  }

  async shutdown(): Promise<void> {
    await unary(this.admin_.shutdown({}));
  }

  close(): void {}
}

/** Wrap a query/semantic frame stream in a `QueryHandle`, pulling the leading
 * `Started` frame first so the handle exposes the id up front. */
async function streamFrames(
  frames: AsyncIterable<QueryResponse>,
): Promise<QueryHandle> {
  const meta: QueryMeta = { columns: [], rowCount: 0, truncated: false };
  const it = frames[Symbol.asyncIterator]();
  let id = "";
  let pending: QueryResponse | undefined;
  let first: IteratorResult<QueryResponse>;
  try {
    first = await it.next();
  } catch (e) {
    throw toPawrlyError(e);
  }
  if (!first.done) {
    if (first.value.payload.case === "started") {
      id = first.value.payload.value.queryId;
    } else {
      pending = first.value;
    }
  }
  return new QueryHandle(id, grpcRows(it, pending, meta), meta);
}

async function* grpcRows(
  it: AsyncIterator<QueryResponse>,
  pending: QueryResponse | undefined,
  meta: QueryMeta,
): AsyncGenerator<Row> {
  if (pending) yield* frameRows(pending, meta);
  for (;;) {
    let res: IteratorResult<QueryResponse>;
    try {
      res = await it.next();
    } catch (e) {
      throw toPawrlyError(e);
    }
    if (res.done) break;
    yield* frameRows(res.value, meta);
  }
}

/** Await a unary RPC, mapping a Connect status error to a `PawrlyError`. */
async function unary<T>(call: Promise<T>): Promise<T> {
  try {
    return await call;
  } catch (e) {
    throw toPawrlyError(e);
  }
}

/** Map a Connect status error to a `PawrlyError`, reading the stable `PAWRLY_*`
 * code the server puts in trailing metadata (`pawrly-error-code`). An in-stream
 * `error` frame already surfaces as a `PawrlyError`; pass those through. */
function requireNamespaceEcho(requested: string | undefined, echoed: string | undefined): void {
  if (requested && requested !== echoed) {
    throw new PawrlyError(
      "PAWRLY_PROTOCOL",
      `server ignored namespace \`${requested}\` — it predates materialize namespaces, so the ` +
        "operation targeted the default namespace instead; upgrade the server",
    );
  }
}

function toPawrlyError(e: unknown): PawrlyError {
  if (e instanceof PawrlyError) return e;
  const ce = ConnectError.from(e);
  const code = ce.metadata.get("pawrly-error-code") ?? "PAWRLY_INTERNAL";
  return new PawrlyError(code, ce.rawMessage);
}

function* frameRows(frame: QueryResponse, meta: QueryMeta): Generator<Row> {
  switch (frame.payload.case) {
    case "ipcStream": {
      const table = tableFromIPC(frame.payload.value);
      if (meta.columns.length === 0) {
        meta.columns = table.schema.fields.map((f) => f.name);
      }
      for (const row of table.toArray()) {
        meta.rowCount += 1;
        yield normalizeRow(row);
      }
      break;
    }
    case "completed":
      meta.rowCount = Number(frame.payload.value.rowsReturned);
      meta.truncated = frame.payload.value.truncated;
      break;
    case "error":
      throw new PawrlyError(frame.payload.value.code, frame.payload.value.message);
    default:
      break;
  }
}

const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER);
const MIN_SAFE = BigInt(Number.MIN_SAFE_INTEGER);

/** Flatten an Arrow row proxy to a plain object, narrowing 64-bit ints (which
 * Arrow decodes as BigInt) to `number` where they fit — so gRPC rows match the
 * REST/JSON shape. Values beyond ±2^53 stay BigInt, where precision needs it. */
function normalizeRow(row: unknown): Row {
  const out: Row = {};
  for (const [key, value] of Object.entries(row as Record<string, unknown>)) {
    out[key] = normalizeValue(value);
  }
  return out;
}

function normalizeValue(v: unknown): unknown {
  if (typeof v === "bigint") return v <= MAX_SAFE && v >= MIN_SAFE ? Number(v) : v;
  if (Array.isArray(v)) return v.map(normalizeValue);
  return v;
}

const OP: Record<FilterOp, PbFilterOp> = {
  equals: PbFilterOp.EQUALS,
  not_equals: PbFilterOp.NOT_EQUALS,
  in: PbFilterOp.IN,
  not_in: PbFilterOp.NOT_IN,
  gt: PbFilterOp.GT,
  gte: PbFilterOp.GTE,
  lt: PbFilterOp.LT,
  lte: PbFilterOp.LTE,
  in_range: PbFilterOp.IN_RANGE,
  contains: PbFilterOp.CONTAINS,
  starts_with: PbFilterOp.STARTS_WITH,
  ends_with: PbFilterOp.ENDS_WITH,
  is_null: PbFilterOp.IS_NULL,
  is_not_null: PbFilterOp.IS_NOT_NULL,
};

function semanticToProto(q: SemanticQuery) {
  return {
    measures: q.measures ?? [],
    dimensions: q.dimensions ?? [],
    filters: (q.filters ?? []).map((f) => ({
      member: f.member,
      op: OP[f.op],
      values: f.values ?? [],
    })),
    orderBy: (q.orderBy ?? []).map((o) => ({ member: o.member, desc: o.desc ?? false })),
    segments: q.segments ?? [],
    limit: q.limit !== undefined ? BigInt(q.limit) : undefined,
    timeZone: q.timeZone,
    params: q.params ?? {},
  };
}

/** Accept `tcp://host:port`, `http(s)://…`, or a bare `host:port`. */
function toHttpUrl(endpoint: string): string {
  if (endpoint.startsWith("http://") || endpoint.startsWith("https://")) {
    return endpoint;
  }
  return `http://${endpoint.replace(/^tcp:\/\//, "")}`;
}

const num = (v: bigint | number): number => Number(v);

/** proto-es enum value → the engine's lowercase string (SourceKind.FILE → "file"). */
function denum(names: unknown, value: number): string {
  return ((names as Record<number, string>)[value] ?? "").toLowerCase();
}

function isoTs(t: Parameters<typeof timestampDate>[0] | undefined): string | undefined {
  return t ? timestampDate(t).toISOString() : undefined;
}

/** proto Duration → milliseconds. */
function durMs(d: { seconds: bigint; nanos: number } | undefined): number {
  return d ? Number(d.seconds) * 1000 + d.nanos / 1e6 : 0;
}

function emptySourceInfo(): SourceInfo {
  return { name: "", kind: "", status: "", tableCount: 0, registeredAt: "" };
}

function pbModelDescription(m: PbModelDescription): SemanticModelDescription {
  return {
    name: m.name,
    description: m.description || undefined,
    source: m.source,
    primaryKey: m.primaryKey,
    dimensions: m.dimensions.map((d) => ({
      name: d.name,
      expr: d.expr,
      type: denum(DimensionType, d.type),
      grains: d.grains.map((g) => denum(TimeGrain, g)),
      description: d.description || undefined,
    })),
    measures: m.measures.map((x) => ({
      name: x.name,
      agg: x.agg,
      customSql: x.customSql || undefined,
      expr: x.expr,
      filters: x.filters,
      format: x.format || undefined,
      description: x.description || undefined,
    })),
    relationships: m.relationships.map((r) => ({
      name: r.name,
      kind: denum(RelationshipKind, r.kind),
      target: r.target,
      on: r.on,
    })),
    segments: m.segments.map((s) => ({
      name: s.name,
      description: s.description || undefined,
      filters: s.filters.map((f) => ({
        member: f.member,
        op: denum(PbFilterOp, f.op) as FilterOp,
        values: f.values,
      })),
    })),
  };
}

function emptyModelDescription(): SemanticModelDescription {
  return {
    name: "",
    source: "",
    primaryKey: [],
    dimensions: [],
    measures: [],
    relationships: [],
    segments: [],
  };
}

function pbTableName(name: string): TableName {
  const i = name.indexOf(".");
  return i < 0
    ? { schema: name, table: "" }
    : { schema: name.slice(0, i), table: name.slice(i + 1) };
}

function pbSourceInfo(s: PbSourceInfo): SourceInfo {
  return {
    name: s.name,
    kind: denum(SourceKind, s.kind),
    status: denum(SourceStatus, s.status),
    statusDetail: s.statusDetail || undefined,
    subKind: s.subKind,
    tableCount: num(s.tableCount),
    registeredAt: isoTs(s.registeredAt) ?? "",
  };
}

function pbTableInfo(t: PbTableInfo): TableInfo {
  return {
    name: t.name ? { schema: t.name.schema, table: t.name.table } : { schema: "", table: "" },
    kind: denum(SourceKind, t.kind),
    description: t.description || undefined,
    rowCountEstimate: t.rowCountEstimate !== undefined ? num(t.rowCountEstimate) : undefined,
    cached: t.cached,
    requiredFilters: t.requiredFilters,
  };
}

function pbColumnSpec(c: PbColumnSpec): ColumnSpec {
  return {
    name: c.name,
    dataType: c.dataType,
    nullable: c.nullable,
    description: c.description || undefined,
    isFilterPushable: c.isFilterPushable,
    isRequiredFilter: c.isRequiredFilter,
  };
}

function pbCacheEntry(e: PbCacheEntryInfo): CacheEntryInfo {
  return {
    name: e.name ? { schema: e.name.schema, table: e.name.table } : { schema: "", table: "" },
    mode: denum(CacheMode, e.mode),
    writtenAt: isoTs(e.writtenAt) ?? "",
    expiresAt: isoTs(e.expiresAt),
    rowCount: num(e.rowCount),
    sizeBytes: num(e.sizeBytes),
    fileCount: num(e.fileCount),
  };
}

function pbFunctionInfo(f: PbFunctionInfo): FunctionInfo {
  return {
    namespace: f.namespace,
    name: f.name,
    kind: f.kind,
    builtin: f.builtin,
    signature: f.signature,
    description: f.description,
  };
}

function pbFunctionDescription(f: PbFunctionDescription): FunctionDescription {
  return {
    namespace: f.namespace,
    name: f.name,
    kind: f.kind,
    builtin: f.builtin,
    signature: f.signature,
    description: f.description,
    wiki: f.wiki,
    examples: f.examples,
    args: f.args.map((a) => ({
      name: a.name,
      type: a.type,
      required: a.required,
      default: a.default,
      description: a.description,
      toolArg: a.toolArg,
    })),
    returns: f.returns.map((c) => ({
      name: c.name,
      type: c.type,
      source: c.source,
      description: c.description,
    })),
  };
}

function emptyTableInfo(): TableInfo {
  return { name: { schema: "", table: "" }, kind: "", cached: false, requiredFilters: [] };
}

function emptyFunctionDescription(): FunctionDescription {
  return {
    namespace: "",
    name: "",
    kind: "",
    builtin: false,
    signature: "",
    examples: [],
    args: [],
    returns: [],
  };
}
