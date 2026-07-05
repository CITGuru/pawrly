import type { QueryHandle } from "./query.js";
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
} from "./result.js";

export interface Transport {
  readonly name: string;
  query(
    sql: string,
    params: Record<string, string>,
    limit?: number,
  ): Promise<QueryHandle>;
  semanticQuery(q: SemanticQuery): Promise<QueryHandle>;
  explain(sql: string, analyze: boolean): Promise<string>;
  cancel(queryId: string): Promise<boolean>;
  materialize(name: string, spec: MaterializeSpec): Promise<MaterializeOutcome>;
  listSources(): Promise<SourceInfo[]>;
  listTables(source?: string, nameGlob?: string): Promise<TableInfo[]>;
  describeTable(name: string): Promise<TableDescription>;
  schemaSnapshot(sources?: string[], compact?: boolean): Promise<CatalogSnapshot>;
  cacheEntries(): Promise<CacheEntryInfo[]>;
  listFunctions(): Promise<FunctionInfo[]>;
  describeFunction(namespace: string, name: string): Promise<FunctionDescription>;
  listSemanticModels(): Promise<SemanticModelInfo[]>;
  describeSemanticModel(name: string): Promise<SemanticModelDescription>;
  addSource(def: SourceDef): Promise<SourceInfo>;
  removeSource(name: string): Promise<boolean>;
  testSource(name: string): Promise<SourceTestReport>;
  reloadConfig(): Promise<ReloadReport>;
  refreshCatalog(source?: string): Promise<RefreshCatalogOutcome>;
  refreshTable(name: string): Promise<RefreshOutcome>;
  invalidateCache(name: string): Promise<boolean>;
  vacuumCache(): Promise<VacuumReport>;
  dropMaterialized(name: string): Promise<boolean>;
  health(): Promise<HealthReport>;
  shutdown(): Promise<void>;
  close(): void;
}
