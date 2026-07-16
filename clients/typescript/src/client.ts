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
import type { Transport } from "./transport.js";
import { GrpcTransport } from "./transports/grpc.js";
import { LocalTransport, type LocalOptions } from "./transports/local.js";
import { RestTransport } from "./transports/rest.js";

export type TransportConfig =
  | { transport: "grpc"; endpoint: string; bearer?: string }
  | { transport: "rest"; baseUrl: string; bearer?: string };

/** One `EngineService` surface; every method is identical regardless of wire. */
export class PawrlyClient {
  private readonly t: Transport;

  constructor(config: TransportConfig | Transport) {
    if ("transport" in config) {
      this.t =
        config.transport === "grpc"
          ? new GrpcTransport(config.endpoint, config.bearer)
          : new RestTransport(config.baseUrl, config.bearer);
    } else {
      this.t = config;
    }
  }

  /** Run the engine in a `pawrly console` child this client owns; `close()` stops it. */
  static async local(opts?: LocalOptions): Promise<PawrlyClient> {
    return new PawrlyClient(await LocalTransport.create(opts));
  }

  get transport(): string {
    return this.t.name;
  }

  query(
    sql: string,
    opts?: { params?: Record<string, string>; limit?: number },
  ): Promise<QueryHandle> {
    return this.t.query(sql, opts?.params ?? {}, opts?.limit);
  }

  semanticQuery(q: SemanticQuery): Promise<QueryHandle> {
    return this.t.semanticQuery(q);
  }

  explain(sql: string, analyze = false): Promise<string> {
    return this.t.explain(sql, analyze);
  }

  cancel(queryId: string): Promise<boolean> {
    return this.t.cancel(queryId);
  }

  materialize(
    name: string,
    spec: MaterializeSpec,
    namespace?: string,
  ): Promise<MaterializeOutcome> {
    return this.t.materialize(name, spec, namespace);
  }

  listSources(): Promise<SourceInfo[]> {
    return this.t.listSources();
  }

  listTables(source?: string, nameGlob?: string): Promise<TableInfo[]> {
    return this.t.listTables(source, nameGlob);
  }

  describeTable(name: string): Promise<TableDescription> {
    return this.t.describeTable(name);
  }

  schemaSnapshot(sources?: string[], compact?: boolean): Promise<CatalogSnapshot> {
    return this.t.schemaSnapshot(sources, compact);
  }

  cacheEntries(namespace?: string): Promise<CacheEntryInfo[]> {
    return this.t.cacheEntries(namespace);
  }

  listFunctions(): Promise<FunctionInfo[]> {
    return this.t.listFunctions();
  }

  describeFunction(namespace: string, name: string): Promise<FunctionDescription> {
    return this.t.describeFunction(namespace, name);
  }

  listSemanticModels(): Promise<SemanticModelInfo[]> {
    return this.t.listSemanticModels();
  }

  describeSemanticModel(name: string): Promise<SemanticModelDescription> {
    return this.t.describeSemanticModel(name);
  }

  addSource(def: SourceDef): Promise<SourceInfo> {
    return this.t.addSource(def);
  }

  removeSource(name: string): Promise<boolean> {
    return this.t.removeSource(name);
  }

  testSource(name: string): Promise<SourceTestReport> {
    return this.t.testSource(name);
  }

  reloadConfig(): Promise<ReloadReport> {
    return this.t.reloadConfig();
  }

  refreshCatalog(source?: string): Promise<RefreshCatalogOutcome> {
    return this.t.refreshCatalog(source);
  }

  refreshTable(name: string): Promise<RefreshOutcome> {
    return this.t.refreshTable(name);
  }

  invalidateCache(name: string): Promise<boolean> {
    return this.t.invalidateCache(name);
  }

  vacuumCache(): Promise<VacuumReport> {
    return this.t.vacuumCache();
  }

  dropMaterialized(name: string, namespace?: string): Promise<boolean> {
    return this.t.dropMaterialized(name, namespace);
  }

  health(): Promise<HealthReport> {
    return this.t.health();
  }

  shutdown(): Promise<void> {
    return this.t.shutdown();
  }

  close(): void {
    this.t.close();
  }
}
