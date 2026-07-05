export interface QueryResult {
  columns: string[];
  rows: Array<Record<string, unknown>>;
  rowCount: number;
  /** True when rows existed beyond the limit; always `false` over REST (NDJSON
   * carries no completion envelope). */
  truncated: boolean;
}

export interface HealthReport {
  ok: boolean;
  version: string;
}

/** Currently only the `query` origin (persist a SQL result). */
export type MaterializeSpec = {
  kind: "query";
  sql: string;
  params?: Record<string, string>;
};

export interface MaterializeOutcome {
  name: { schema: string; table: string };
  filePath: string;
  rowCount: number;
  sizeBytes: number;
}

export type FilterOp =
  | "equals"
  | "not_equals"
  | "in"
  | "not_in"
  | "gt"
  | "gte"
  | "lt"
  | "lte"
  | "in_range"
  | "contains"
  | "starts_with"
  | "ends_with"
  | "is_null"
  | "is_not_null";

export interface SemanticFilter {
  member: string;
  op: FilterOp;
  values?: string[];
}

export interface SemanticOrder {
  member: string;
  desc?: boolean;
}

export interface SemanticQuery {
  measures?: string[];
  dimensions?: string[];
  filters?: SemanticFilter[];
  orderBy?: SemanticOrder[];
  segments?: string[];
  limit?: number;
  timeZone?: string;
  params?: Record<string, string>;
}

export interface TableName {
  schema: string;
  table: string;
}

export interface SourceInfo {
  name: string;
  /** file | http | mcp | postgres | mysql | sqlite | duckdb | snowflake | iceberg | ducklake | delta */
  kind: string;
  /** ok | unavailable */
  status: string;
  statusDetail?: string;
  subKind?: string;
  tableCount: number;
  registeredAt: string;
}

export interface TableInfo {
  name: TableName;
  kind: string;
  description?: string;
  rowCountEstimate?: number;
  cached: boolean;
  requiredFilters: string[];
}

export interface ColumnSpec {
  name: string;
  /** Arrow type as a string, e.g. `"Int64"`, `"Decimal128(18, 2)"`. */
  dataType: string;
  nullable: boolean;
  description?: string;
  isFilterPushable: boolean;
  isRequiredFilter: boolean;
}

export interface TableDescription {
  table: TableInfo;
  columns: ColumnSpec[];
  pushableFilterColumns: string[];
  examples: string[];
  wiki?: string;
}

export interface TableSummary {
  name: string;
  /** single-line `"col1 type, col2 type, ..."` form. */
  columns: string;
  requiredFilters: string[];
}

export interface SchemaSummary {
  name: string;
  kind: string;
  tables: TableSummary[];
}

export interface CatalogSnapshot {
  schemas: SchemaSummary[];
}

export interface CacheEntryInfo {
  name: TableName;
  /** none | ttl | refresh | cron | append */
  mode: string;
  writtenAt: string;
  expiresAt?: string;
  rowCount: number;
  sizeBytes: number;
  fileCount: number;
}

export interface FunctionInfo {
  namespace: string;
  name: string;
  kind: string;
  builtin: boolean;
  signature: string;
  description?: string;
}

export interface FunctionArg {
  name: string;
  type: string;
  required: boolean;
  default?: string;
  description?: string;
  toolArg?: string;
}

export interface FunctionColumn {
  name: string;
  type: string;
  source?: string;
  description?: string;
}

export interface FunctionDescription {
  namespace: string;
  name: string;
  kind: string;
  builtin: boolean;
  signature: string;
  description?: string;
  wiki?: string;
  examples: string[];
  args: FunctionArg[];
  returns: FunctionColumn[];
}

export interface SemanticModelInfo {
  name: string;
  description?: string;
  source: string;
  dimensionCount: number;
  measureCount: number;
}

/** A source definition for `addSource()` — the YAML config as an object. */
export interface SourceDef {
  name: string;
  kind: string;
  config?: Record<string, unknown>;
  description?: string;
  [key: string]: unknown;
}

export interface SourceTestReport {
  name: string;
  ok: boolean;
  latencyMs: number;
  detail?: string;
}

export interface ReloadReport {
  sourcesAdded: number;
  sourcesRemoved: number;
  sourcesChanged: number;
}

export interface RefreshCatalogOutcome {
  sourcesRefreshed: number;
  tablesDiscovered: number;
}

export interface RefreshOutcome {
  table: TableName;
  rowsWritten: number;
  sizeBytes: number;
  elapsedMs: number;
  expiresAt?: string;
}

export interface VacuumReport {
  entriesRemoved: number;
  filesRemoved: number;
  bytesReclaimed: number;
}

export interface SemanticDimension {
  name: string;
  expr: string;
  /** string | number | time | bool */
  type: string;
  /** time grains (hour | day | week | month | quarter | year); only for `time` */
  grains: string[];
  description?: string;
}

export interface SemanticMeasure {
  name: string;
  /** sum | count | count_distinct | avg | min | max | custom */
  agg: string;
  /** the SQL when `agg === "custom"` */
  customSql?: string;
  expr: string;
  filters: string[];
  format?: string;
  description?: string;
}

export interface SemanticRelationship {
  name: string;
  /** many_to_one | one_to_many | one_to_one */
  kind: string;
  target: string;
  on: string;
}

export interface SemanticSegment {
  name: string;
  description?: string;
  filters: SemanticFilter[];
}

export interface SemanticModelDescription {
  name: string;
  description?: string;
  source: string;
  primaryKey: string[];
  dimensions: SemanticDimension[];
  measures: SemanticMeasure[];
  relationships: SemanticRelationship[];
  segments: SemanticSegment[];
}
