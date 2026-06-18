import type { Timestamp } from "@bufbuild/protobuf/wkt";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import { SourceKind, SourceStatus, CacheMode } from "@/gen/pawrly/v1/common_pb";
import {
  DimensionType,
  RelationshipKind,
  TimeGrain,
} from "@/gen/pawrly/v1/semantic_pb";

/** Best-effort human message from a thrown value (Connect errors included). */
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

const SOURCE_KIND_LABELS: Record<number, string> = {
  [SourceKind.UNSPECIFIED]: "unknown",
  [SourceKind.HTTP]: "http",
  [SourceKind.FILE]: "file",
  [SourceKind.POSTGRES]: "postgres",
  [SourceKind.MYSQL]: "mysql",
  [SourceKind.SQLITE]: "sqlite",
  [SourceKind.DUCKDB]: "duckdb",
  [SourceKind.SNOWFLAKE]: "snowflake",
  [SourceKind.ICEBERG]: "iceberg",
  [SourceKind.DELTA]: "delta",
  [SourceKind.DUCKLAKE]: "ducklake",
  [SourceKind.MCP]: "mcp",
};

export function sourceKindLabel(kind: SourceKind): string {
  return SOURCE_KIND_LABELS[kind] ?? "unknown";
}

const CACHE_MODE_LABELS: Record<number, string> = {
  [CacheMode.UNSPECIFIED]: "—",
  [CacheMode.NONE]: "none",
  [CacheMode.TTL]: "ttl",
  [CacheMode.REFRESH]: "refresh",
  [CacheMode.CRON]: "cron",
  [CacheMode.APPEND]: "append",
};

export function cacheModeLabel(mode: CacheMode): string {
  return CACHE_MODE_LABELS[mode] ?? "—";
}

export function sourceStatusOk(status: SourceStatus): boolean {
  return status === SourceStatus.OK;
}

export function formatBytes(bytes: bigint | number): string {
  let n = typeof bytes === "bigint" ? Number(bytes) : bytes;
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function formatCount(n: bigint | number): string {
  const v = typeof n === "bigint" ? n : BigInt(Math.trunc(n));
  return v.toLocaleString();
}

export function formatTimestamp(ts: Timestamp | undefined): string {
  if (!ts) return "—";
  try {
    return timestampDate(ts).toLocaleString();
  } catch {
    return "—";
  }
}

export function tableNameString(name?: {
  schema: string;
  table: string;
}): string {
  if (!name) return "";
  return name.schema ? `${name.schema}.${name.table}` : name.table;
}

const DIMENSION_TYPE_LABELS: Record<number, string> = {
  [DimensionType.UNSPECIFIED]: "—",
  [DimensionType.STRING]: "string",
  [DimensionType.NUMBER]: "number",
  [DimensionType.TIME]: "time",
  [DimensionType.BOOL]: "bool",
};

export function dimensionTypeLabel(t: DimensionType): string {
  return DIMENSION_TYPE_LABELS[t] ?? "—";
}

const RELATIONSHIP_KIND_LABELS: Record<number, string> = {
  [RelationshipKind.UNSPECIFIED]: "—",
  [RelationshipKind.MANY_TO_ONE]: "many → one",
  [RelationshipKind.ONE_TO_MANY]: "one → many",
  [RelationshipKind.ONE_TO_ONE]: "one → one",
};

export function relationshipKindLabel(k: RelationshipKind): string {
  return RELATIONSHIP_KIND_LABELS[k] ?? "—";
}

const TIME_GRAIN_LABELS: Record<number, string> = {
  [TimeGrain.UNSPECIFIED]: "",
  [TimeGrain.HOUR]: "hour",
  [TimeGrain.DAY]: "day",
  [TimeGrain.WEEK]: "week",
  [TimeGrain.MONTH]: "month",
  [TimeGrain.QUARTER]: "quarter",
  [TimeGrain.YEAR]: "year",
};

export function timeGrainLabel(g: TimeGrain): string {
  return TIME_GRAIN_LABELS[g] ?? "";
}

/**
 * Build a trace deep-link from a user-supplied template. Substitutes
 * `{traceId}` / `{trace_id}` (case-insensitive); if the template has no
 * placeholder, the id is appended. Returns null when either input is empty.
 */
export function buildTraceUrl(
  template: string,
  traceId: string,
): string | null {
  if (!template || !traceId) return null;
  const id = encodeURIComponent(traceId);
  if (/\{trace_?id\}/i.test(template)) {
    return template.replace(/\{trace_?id\}/gi, id);
  }
  return template + id;
}
