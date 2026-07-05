import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { PawrlyError } from "../errors.js";
import type { QueryHandle } from "../query.js";
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
import { RestTransport } from "./rest.js";

export interface LocalOptions {
  config?: string;
  home?: string;
  binary?: string;
}

/** Spawns a `pawrly console` child on a private loopback port and talks to it
 * over REST — an engine this client owns and tears down. */
export class LocalTransport implements Transport {
  readonly name = "local";

  private constructor(
    private readonly rest: RestTransport,
    private readonly proc: ChildProcess,
    private readonly tmp: string,
  ) {}

  static async create(opts?: LocalOptions): Promise<LocalTransport> {
    const binary = opts?.binary ?? "pawrly";
    const tmp = mkdtempSync(join(tmpdir(), "pawrly-local-"));
    let config = opts?.config;
    if (!config) {
      config = join(tmp, "pawrly.yaml");
      writeFileSync(config, "version: 1\n");
    }
    const port = await freePort();
    const args: string[] = [];
    if (opts?.home) args.push("--home", opts.home);
    args.push("--config", config, "console", "--addr", `127.0.0.1:${port}`);
    const proc = spawn(binary, args, { stdio: ["ignore", "ignore", "pipe"] });

    const rest = new RestTransport(`http://127.0.0.1:${port}`);
    const self = new LocalTransport(rest, proc, tmp);

    const deadline = Date.now() + 10_000;
    for (;;) {
      if (proc.exitCode !== null) {
        self.cleanup();
        throw new PawrlyError("PAWRLY_INTERNAL", `pawrly console exited (${proc.exitCode})`);
      }
      try {
        if ((await rest.health()).ok) break;
      } catch {
        /* not up yet */
      }
      if (Date.now() > deadline) {
        self.terminate();
        self.cleanup();
        throw new PawrlyError("PAWRLY_INTERNAL", "pawrly console never became healthy");
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    return self;
  }

  query(sql: string, params: Record<string, string>, limit?: number): Promise<QueryHandle> {
    return this.rest.query(sql, params, limit);
  }
  semanticQuery(q: SemanticQuery): Promise<QueryHandle> {
    return this.rest.semanticQuery(q);
  }
  explain(sql: string, analyze: boolean): Promise<string> {
    return this.rest.explain(sql, analyze);
  }
  cancel(queryId: string): Promise<boolean> {
    return this.rest.cancel(queryId);
  }
  materialize(name: string, spec: MaterializeSpec): Promise<MaterializeOutcome> {
    return this.rest.materialize(name, spec);
  }
  listSources(): Promise<SourceInfo[]> {
    return this.rest.listSources();
  }
  listTables(source?: string, nameGlob?: string): Promise<TableInfo[]> {
    return this.rest.listTables(source, nameGlob);
  }
  describeTable(name: string): Promise<TableDescription> {
    return this.rest.describeTable(name);
  }
  schemaSnapshot(sources?: string[], compact?: boolean): Promise<CatalogSnapshot> {
    return this.rest.schemaSnapshot(sources, compact);
  }
  cacheEntries(): Promise<CacheEntryInfo[]> {
    return this.rest.cacheEntries();
  }
  listFunctions(): Promise<FunctionInfo[]> {
    return this.rest.listFunctions();
  }
  describeFunction(namespace: string, name: string): Promise<FunctionDescription> {
    return this.rest.describeFunction(namespace, name);
  }
  listSemanticModels(): Promise<SemanticModelInfo[]> {
    return this.rest.listSemanticModels();
  }
  describeSemanticModel(name: string): Promise<SemanticModelDescription> {
    return this.rest.describeSemanticModel(name);
  }
  addSource(def: SourceDef): Promise<SourceInfo> {
    return this.rest.addSource(def);
  }
  removeSource(name: string): Promise<boolean> {
    return this.rest.removeSource(name);
  }
  testSource(name: string): Promise<SourceTestReport> {
    return this.rest.testSource(name);
  }
  reloadConfig(): Promise<ReloadReport> {
    return this.rest.reloadConfig();
  }
  refreshCatalog(source?: string): Promise<RefreshCatalogOutcome> {
    return this.rest.refreshCatalog(source);
  }
  refreshTable(name: string): Promise<RefreshOutcome> {
    return this.rest.refreshTable(name);
  }
  invalidateCache(name: string): Promise<boolean> {
    return this.rest.invalidateCache(name);
  }
  vacuumCache(): Promise<VacuumReport> {
    return this.rest.vacuumCache();
  }
  dropMaterialized(name: string): Promise<boolean> {
    return this.rest.dropMaterialized(name);
  }
  health(): Promise<HealthReport> {
    return this.rest.health();
  }
  shutdown(): Promise<void> {
    return this.rest.shutdown();
  }

  private terminate(): void {
    if (this.proc.exitCode === null) this.proc.kill("SIGTERM");
  }
  private cleanup(): void {
    rmSync(this.tmp, { recursive: true, force: true });
  }
  close(): void {
    this.rest.close();
    this.terminate();
    this.cleanup();
  }
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      const port = addr && typeof addr === "object" ? addr.port : 0;
      srv.close(() => resolve(port));
    });
  });
}
