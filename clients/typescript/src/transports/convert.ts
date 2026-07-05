// Build result objects from the REST JSON shapes (snake_case, 1:1 with the
// engine's serde types) into the SDK's camelCase interfaces.
import type {
  CacheEntryInfo,
  CatalogSnapshot,
  ColumnSpec,
  FilterOp,
  FunctionArg,
  FunctionColumn,
  FunctionDescription,
  FunctionInfo,
  RefreshCatalogOutcome,
  RefreshOutcome,
  ReloadReport,
  SchemaSummary,
  SemanticDimension,
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
} from "../result.js";

type J = Record<string, unknown>;
const obj = (v: unknown): J => (v ?? {}) as J;
const arr = (v: unknown): J[] => (Array.isArray(v) ? (v as J[]) : []);
const str = (v: unknown): string | undefined => (typeof v === "string" ? v : undefined);
const int = (v: unknown): number => (typeof v === "number" ? v : 0);

const HUMANTIME_MS: Record<string, number> = {
  ns: 1e-6,
  us: 1e-3,
  "µs": 1e-3,
  ms: 1,
  s: 1000,
  m: 60_000,
  h: 3_600_000,
  d: 86_400_000,
};

/** Parse a humantime duration (`"1s 500ms"`, `"150ms"`) into milliseconds. */
export function humantimeMs(value: unknown): number {
  if (typeof value !== "string") return 0;
  let total = 0;
  for (const [, num, unit] of value.matchAll(/(\d+(?:\.\d+)?)\s*(ns|us|µs|ms|s|m|h|d)/g)) {
    total += parseFloat(num) * (HUMANTIME_MS[unit] ?? 0);
  }
  return total;
}

export function tableName(d: unknown): TableName {
  const o = obj(d);
  return { schema: (o.schema as string) ?? "", table: (o.table as string) ?? "" };
}

export function sourceInfo(d: J): SourceInfo {
  return {
    name: (d.name as string) ?? "",
    kind: (d.kind as string) ?? "",
    status: (d.status as string) ?? "",
    statusDetail: str(d.status_detail),
    subKind: str(d.sub_kind),
    tableCount: (d.table_count as number) ?? 0,
    registeredAt: (d.registered_at as string) ?? "",
  };
}

export function tableInfo(d: J): TableInfo {
  return {
    name: tableName(d.name),
    kind: (d.kind as string) ?? "",
    description: str(d.description),
    rowCountEstimate: d.row_count_estimate as number | undefined,
    cached: (d.cached as boolean) ?? false,
    requiredFilters: (d.required_filters as string[]) ?? [],
  };
}

export function columnSpec(d: J): ColumnSpec {
  return {
    name: (d.name as string) ?? "",
    dataType: (d.data_type as string) ?? "",
    nullable: (d.nullable as boolean) ?? false,
    description: str(d.description),
    isFilterPushable: (d.is_filter_pushable as boolean) ?? false,
    isRequiredFilter: (d.is_required_filter as boolean) ?? false,
  };
}

export function tableDescription(d: J): TableDescription {
  return {
    table: tableInfo(obj(d.table)),
    columns: arr(d.columns).map(columnSpec),
    pushableFilterColumns: (d.pushable_filter_columns as string[]) ?? [],
    examples: (d.examples as string[]) ?? [],
    wiki: str(d.wiki),
  };
}

export function catalogSnapshot(d: J): CatalogSnapshot {
  return {
    schemas: arr(d.schemas).map(
      (s): SchemaSummary => ({
        name: (s.name as string) ?? "",
        kind: (s.kind as string) ?? "",
        tables: arr(s.tables).map(
          (t): TableSummary => ({
            name: (t.name as string) ?? "",
            columns: (t.columns as string) ?? "",
            requiredFilters: (t.required_filters as string[]) ?? [],
          }),
        ),
      }),
    ),
  };
}

export function cacheEntry(d: J): CacheEntryInfo {
  return {
    name: tableName(d.name),
    mode: (d.mode as string) ?? "",
    writtenAt: (d.written_at as string) ?? "",
    expiresAt: str(d.expires_at),
    rowCount: (d.row_count as number) ?? 0,
    sizeBytes: (d.size_bytes as number) ?? 0,
    fileCount: (d.file_count as number) ?? 0,
  };
}

export function functionInfo(d: J): FunctionInfo {
  return {
    namespace: (d.namespace as string) ?? "",
    name: (d.name as string) ?? "",
    kind: (d.kind as string) ?? "",
    builtin: (d.builtin as boolean) ?? false,
    signature: (d.signature as string) ?? "",
    description: str(d.description),
  };
}

export function functionDescription(d: J): FunctionDescription {
  return {
    namespace: (d.namespace as string) ?? "",
    name: (d.name as string) ?? "",
    kind: (d.kind as string) ?? "",
    builtin: (d.builtin as boolean) ?? false,
    signature: (d.signature as string) ?? "",
    description: str(d.description),
    wiki: str(d.wiki),
    examples: (d.examples as string[]) ?? [],
    args: arr(d.args).map(
      (a): FunctionArg => ({
        name: (a.name as string) ?? "",
        type: (a.type as string) ?? "",
        required: (a.required as boolean) ?? false,
        default: str(a.default),
        description: str(a.description),
        toolArg: str(a.tool_arg),
      }),
    ),
    returns: arr(d.returns).map(
      (c): FunctionColumn => ({
        name: (c.name as string) ?? "",
        type: (c.type as string) ?? "",
        source: str(c.source),
        description: str(c.description),
      }),
    ),
  };
}

export function semanticModelInfo(d: J): SemanticModelInfo {
  return {
    name: (d.name as string) ?? "",
    description: str(d.description),
    source: (d.source as string) ?? "",
    dimensionCount: (d.dimension_count as number) ?? 0,
    measureCount: (d.measure_count as number) ?? 0,
  };
}

export function sourceTestReport(d: J): SourceTestReport {
  return {
    name: (d.name as string) ?? "",
    ok: (d.ok as boolean) ?? false,
    latencyMs: humantimeMs(d.latency),
    detail: str(d.detail),
  };
}

export function reloadReport(d: J): ReloadReport {
  return {
    sourcesAdded: int(d.sources_added),
    sourcesRemoved: int(d.sources_removed),
    sourcesChanged: int(d.sources_changed),
  };
}

export function refreshCatalogOutcome(d: J): RefreshCatalogOutcome {
  return {
    sourcesRefreshed: int(d.sources_refreshed),
    tablesDiscovered: int(d.tables_discovered),
  };
}

export function refreshOutcome(d: J): RefreshOutcome {
  return {
    table: tableName(d.table),
    rowsWritten: int(d.rows_written),
    sizeBytes: int(d.size_bytes),
    elapsedMs: humantimeMs(d.elapsed),
    expiresAt: str(d.expires_at),
  };
}

export function vacuumReport(d: J): VacuumReport {
  return {
    entriesRemoved: int(d.entries_removed),
    filesRemoved: int(d.files_removed),
    bytesReclaimed: int(d.bytes_reclaimed),
  };
}

/** MeasureAgg is a bare string for unit variants, or `{ custom: { sql } }`. */
function measureAgg(agg: unknown): { agg: string; customSql?: string } {
  if (typeof agg === "string") return { agg };
  if (agg && typeof agg === "object" && "custom" in agg) {
    return { agg: "custom", customSql: str((agg as J).custom && obj((agg as J).custom).sql) };
  }
  return { agg: "" };
}

export function semanticModelDescription(d: J): SemanticModelDescription {
  return {
    name: (d.name as string) ?? "",
    description: str(d.description),
    source: (d.source as string) ?? "",
    primaryKey: (d.primary_key as string[]) ?? [],
    dimensions: arr(d.dimensions).map(
      (x): SemanticDimension => ({
        name: (x.name as string) ?? "",
        expr: (x.expr as string) ?? "",
        type: (x.type as string) ?? "",
        grains: (x.grains as string[]) ?? [],
        description: str(x.description),
      }),
    ),
    measures: arr(d.measures).map((x): SemanticMeasure => {
      const { agg, customSql } = measureAgg(x.agg);
      return {
        name: (x.name as string) ?? "",
        agg,
        customSql,
        expr: (x.expr as string) ?? "",
        filters: (x.filters as string[]) ?? [],
        format: str(x.format),
        description: str(x.description),
      };
    }),
    relationships: arr(d.relationships).map(
      (x): SemanticRelationship => ({
        name: (x.name as string) ?? "",
        kind: (x.kind as string) ?? "",
        target: (x.target as string) ?? "",
        on: (x.on as string) ?? "",
      }),
    ),
    segments: arr(d.segments).map(
      (x): SemanticSegment => ({
        name: (x.name as string) ?? "",
        description: str(x.description),
        filters: arr(x.filters).map((f) => ({
          member: (f.member as string) ?? "",
          op: f.op as FilterOp,
          values: f.values as string[] | undefined,
        })),
      }),
    ),
  };
}
