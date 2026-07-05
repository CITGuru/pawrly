import { useMemo, useState, type ReactNode } from "react";
import { ChevronLeft, Loader2, Search, Table2, Trash2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { streamQuery } from "@/lib/query";
import {
  errMsg,
  formatCount,
  formatTimestamp,
  sourceKindLabel,
  sourceStatusOk,
  tableNameString,
} from "@/lib/format";
import { connectorForSource } from "@/catalog";
import type { SourceInfo, TableInfo } from "@/gen/pawrly/v1/common_pb";
import { ConnectorIcon } from "@/components/ConnectorIcon";
import { StatusDot } from "@/components/dataviz";
import { StatRow, StatTile } from "@/components/StatTile";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export function SourceDetailPage({
  source,
  onBack,
  onRemoved,
  onOpenSql,
}: {
  source: SourceInfo;
  onBack: () => void;
  onRemoved: () => void;
  onOpenSql: (sql: string) => void;
}) {
  const connector = connectorForSource(source.name, sourceKindLabel(source.kind));
  const ok = sourceStatusOk(source.status);

  return (
    <div className="space-y-5">
      <button
        onClick={onBack}
        className="text-muted-foreground hover:text-foreground flex items-center gap-1 text-sm"
      >
        <ChevronLeft className="size-4" /> Connectors
      </button>

      <div className="flex items-start justify-between gap-4">
        <div className="flex items-center gap-3">
          <ConnectorIcon
            seed={connector?.id ?? source.name}
            label={connector?.label ?? source.name}
            className="size-10"
          />
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">{source.name}</h1>
              <Badge variant={ok ? "success" : "destructive"}>
                {ok ? "Connected" : "Unavailable"}
              </Badge>
            </div>
            <p className="text-muted-foreground mt-0.5 text-sm">
              {sourceKindLabel(source.kind)}
              {source.subKind ? ` · ${source.subKind}` : ""}
              {` · ${formatCount(source.tableCount)} tables`}
            </p>
          </div>
        </div>
      </div>

      <Tabs defaultValue="schema" className="gap-4">
        <TabsList>
          <TabsTrigger value="schema">Schema</TabsTrigger>
          <TabsTrigger value="usage">Usage</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
        </TabsList>

        <TabsContent value="schema">
          <SchemaBrowser source={source.name} onOpenSql={onOpenSql} />
        </TabsContent>
        <TabsContent value="usage">
          <UsageTab source={source.name} />
        </TabsContent>
        <TabsContent value="settings">
          <SettingsTab source={source} onRemoved={onRemoved} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

/* ------------------------------------------------------------------ Schema */

function SchemaBrowser({
  source,
  onOpenSql,
}: {
  source: string;
  onOpenSql: (sql: string) => void;
}) {
  const { catalog } = useClients();
  const state = useAsync(
    () => catalog.listTables({ source }).then((r) => r.tables),
    [source],
  );
  const [q, setQ] = useState("");
  const [selected, setSelected] = useState<TableInfo | null>(null);

  const tables = state.data ?? [];
  const filtered = useMemo(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return tables;
    return tables.filter((t) =>
      tableNameString(t.name).toLowerCase().includes(needle),
    );
  }, [tables, q]);

  const active = selected ?? filtered[0] ?? null;

  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-[18rem_1fr]">
      <div className="bg-card flex max-h-[60vh] flex-col rounded-lg border">
        <div className="relative border-b p-2">
          <Search className="text-muted-foreground absolute top-1/2 left-4 size-4 -translate-y-1/2" />
          <Input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Search tables…"
            className="h-8 pl-8"
          />
        </div>
        <div className="overflow-y-auto p-1">
          {state.loading && !state.data ? (
            <div className="text-muted-foreground flex items-center gap-2 p-3 text-sm">
              <Loader2 className="size-4 animate-spin" /> Loading…
            </div>
          ) : filtered.length === 0 ? (
            <p className="text-muted-foreground p-3 text-sm">No tables.</p>
          ) : (
            filtered.map((t) => {
              const ref = tableNameString(t.name);
              const isActive = active && tableNameString(active.name) === ref;
              return (
                <button
                  key={ref}
                  onClick={() => setSelected(t)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors",
                    isActive
                      ? "bg-accent text-accent-foreground font-medium"
                      : "hover:bg-muted/60",
                  )}
                >
                  <Table2 className="text-muted-foreground size-4 shrink-0" />
                  <span className="truncate font-mono text-xs">{ref}</span>
                </button>
              );
            })
          )}
        </div>
      </div>

      <div className="min-w-0">
        {state.error ? (
          <p className="text-destructive font-mono text-sm break-all">{state.error}</p>
        ) : active ? (
          <TablePreview source={source} table={active} onOpenSql={onOpenSql} />
        ) : (
          <p className="text-muted-foreground p-3 text-sm">
            Select a table to preview.
          </p>
        )}
      </div>
    </div>
  );
}

function TablePreview({
  table,
  onOpenSql,
}: {
  source: string;
  table: TableInfo;
  onOpenSql: (sql: string) => void;
}) {
  const { catalog, query } = useClients();
  const ref = tableNameString(table.name);
  const requiresFilter = table.requiredFilters.length > 0;
  const previewSql = `SELECT * FROM ${ref} LIMIT 50`;

  const cols = useAsync(
    () => (table.name ? catalog.describeTable({ name: table.name }) : Promise.resolve(null)),
    [ref],
  );
  const preview = useAsync(
    () => (requiresFilter ? Promise.resolve(null) : streamQuery(query, previewSql, 50, {})),
    [ref],
  );

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h2 className="flex items-center gap-2 font-mono text-base font-semibold">
          <Table2 className="text-muted-foreground size-4" /> {ref}
        </h2>
        <Button variant="outline" size="sm" onClick={() => onOpenSql(previewSql)}>
          Open in SQL
        </Button>
      </div>
      {table.description ? (
        <p className="text-muted-foreground text-sm">{table.description}</p>
      ) : null}

      <Tabs defaultValue="preview" className="gap-3">
        <TabsList>
          <TabsTrigger value="preview">Preview</TabsTrigger>
          <TabsTrigger value="columns">
            Columns{cols.data ? ` (${cols.data.columns.length})` : ""}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="preview">
          {requiresFilter ? (
            <p className="text-muted-foreground bg-card rounded-lg border p-4 text-sm">
              This table requires filters ({table.requiredFilters.join(", ")}), so it
              can't be previewed unfiltered. Use “Open in SQL”.
            </p>
          ) : preview.loading && !preview.data ? (
            <div className="text-muted-foreground flex items-center gap-2 p-3 text-sm">
              <Loader2 className="size-4 animate-spin" /> Running preview…
            </div>
          ) : preview.error ? (
            <p className="text-destructive font-mono text-xs break-all">{preview.error}</p>
          ) : preview.data ? (
            <ResultGrid columns={preview.data.columns} rows={preview.data.rows} />
          ) : null}
        </TabsContent>

        <TabsContent value="columns">
          {cols.loading && !cols.data ? (
            <div className="text-muted-foreground flex items-center gap-2 p-3 text-sm">
              <Loader2 className="size-4 animate-spin" /> Loading columns…
            </div>
          ) : cols.data ? (
            <div className="max-h-[50vh] overflow-auto rounded-lg border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Column</TableHead>
                    <TableHead>Type</TableHead>
                    <TableHead>Null</TableHead>
                    <TableHead>Description</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {cols.data.columns.map((c) => (
                    <TableRow key={c.name}>
                      <TableCell className="font-mono font-medium">{c.name}</TableCell>
                      <TableCell className="text-muted-foreground font-mono">
                        {c.dataType}
                      </TableCell>
                      <TableCell className="text-muted-foreground">
                        {c.nullable ? "yes" : "no"}
                      </TableCell>
                      <TableCell className="text-muted-foreground max-w-xs truncate">
                        {c.description || "—"}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          ) : null}
        </TabsContent>
      </Tabs>
    </div>
  );
}

function ResultGrid({ columns, rows }: { columns: string[]; rows: string[][] }) {
  if (columns.length === 0) {
    return <p className="text-muted-foreground p-3 text-sm">No columns.</p>;
  }
  return (
    <div className="max-h-[50vh] overflow-auto rounded-lg border">
      <Table>
        <TableHeader>
          <TableRow>
            {columns.map((c) => (
              <TableHead key={c}>{c}</TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 ? (
            <TableRow>
              <TableCell colSpan={columns.length} className="text-muted-foreground">
                No rows.
              </TableCell>
            </TableRow>
          ) : (
            rows.map((r, i) => (
              <TableRow key={i}>
                {r.map((v, j) => (
                  <TableCell key={j} className="font-mono text-xs">
                    {v}
                  </TableCell>
                ))}
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>
    </div>
  );
}

/* ------------------------------------------------------------------- Usage */

function UsageTab({ source }: { source: string }) {
  const { query } = useClients();
  const safe = source.replace(/'/g, "''").toLowerCase();
  const sql = `SELECT at, operation, status, duration_ms, rows_returned, sql FROM system.activity WHERE lower(sql) LIKE '%${safe}.%' ORDER BY at DESC LIMIT 100`;
  const state = useAsync(() => streamQuery(query, sql, 100, {}), [source]);

  const records = useMemo(() => {
    if (!state.data) return [];
    const { columns, rows } = state.data;
    return rows.map((row) =>
      Object.fromEntries(columns.map((c, i) => [c, row[i] ?? ""])),
    );
  }, [state.data]);

  if (state.error) {
    return (
      <div className="bg-card space-y-2 rounded-lg border p-6">
        <p className="text-sm font-medium">Usage needs the activity log.</p>
        <p className="text-muted-foreground text-sm">
          Enable the table sink in your workspace config (observability.activity).
        </p>
      </div>
    );
  }

  const total = records.length;
  const last = records[0]?.at ?? "";
  const durations = records
    .map((r) => Number(r.duration_ms))
    .filter((n) => Number.isFinite(n));
  const avg =
    durations.length > 0
      ? Math.round(durations.reduce((a, b) => a + b, 0) / durations.length)
      : null;

  return (
    <div className="space-y-4">
      <StatRow>
        <StatTile label="Queries (recent)" value={total} />
        <StatTile
          label="Last queried"
          value={
            <span className="text-base font-normal">
              {last ? new Date(last).toLocaleString() : "—"}
            </span>
          }
        />
        <StatTile label="Avg duration" value={avg != null ? `${avg} ms` : "—"} />
      </StatRow>

      {total === 0 ? (
        <p className="text-muted-foreground bg-card rounded-lg border px-4 py-10 text-center text-sm">
          No recorded queries for this source yet.
        </p>
      ) : (
        <div className="bg-card overflow-hidden rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Operation</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>SQL</TableHead>
                <TableHead className="text-right">Rows</TableHead>
                <TableHead className="text-right">Duration</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {records.map((r, i) => {
                const ok = (r.status || "").toLowerCase() === "ok";
                return (
                  <TableRow key={r.id || i}>
                    <TableCell className="text-muted-foreground tabular-nums">
                      {r.at ? new Date(r.at).toLocaleTimeString() : "—"}
                    </TableCell>
                    <TableCell>{r.operation || "—"}</TableCell>
                    <TableCell>
                      <StatusDot ok={ok} label={r.status || "—"} />
                    </TableCell>
                    <TableCell className="max-w-md truncate font-mono text-xs">
                      {r.sql || "—"}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {r.rows_returned || "—"}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {r.duration_ms ? `${r.duration_ms} ms` : "—"}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}

/* ---------------------------------------------------------------- Settings */

function SettingsTab({
  source,
  onRemoved,
}: {
  source: SourceInfo;
  onRemoved: () => void;
}) {
  const { sources, catalog, query } = useClients();
  const [confirming, setConfirming] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [test, setTest] = useState<
    | { kind: "idle" }
    | { kind: "running" }
    | { kind: "ok"; detail: string }
    | { kind: "err"; detail: string }
  >({ kind: "idle" });
  const [error, setError] = useState<string | null>(null);

  async function handleTest() {
    setTest({ kind: "running" });
    try {
      const tables = (await catalog.listTables({ source: source.name })).tables;
      const probeable = tables.find((t) => t.name && t.requiredFilters.length === 0);
      if (!probeable?.name) {
        setTest({
          kind: "ok",
          detail: tables.length
            ? `Registered — ${tables.length} tables (none probe-able without filters).`
            : "Registered.",
        });
        return;
      }
      const ref = tableNameString(probeable.name);
      const res = await streamQuery(query, `SELECT * FROM ${ref} LIMIT 1`, 1, {});
      setTest({
        kind: "ok",
        detail: `Connected — ${ref} returned ${res.rows.length} row${res.rows.length === 1 ? "" : "s"}.`,
      });
    } catch (e) {
      setTest({ kind: "err", detail: errMsg(e) });
    }
  }

  async function handleRemove() {
    setRemoving(true);
    setError(null);
    try {
      await sources.removeSource({ name: source.name });
      onRemoved();
    } catch (e) {
      setError(errMsg(e));
      setRemoving(false);
    }
  }

  return (
    <div className="max-w-2xl space-y-6">
      <section className="space-y-3">
        <h3 className="text-sm font-semibold">Connection</h3>
        <div className="bg-card grid grid-cols-2 gap-px overflow-hidden rounded-lg border sm:grid-cols-3">
          <Meta label="Kind">{sourceKindLabel(source.kind)}</Meta>
          <Meta label="Status">
            <StatusDot
              ok={sourceStatusOk(source.status)}
              label={sourceStatusOk(source.status) ? "ok" : "unavailable"}
            />
          </Meta>
          <Meta label="Tables">{formatCount(source.tableCount)}</Meta>
          <Meta label="Registered">
            <span className="text-sm font-normal">
              {formatTimestamp(source.registeredAt)}
            </span>
          </Meta>
        </div>
        <div className="flex items-center gap-3">
          <Button variant="outline" onClick={handleTest} disabled={test.kind === "running"}>
            {test.kind === "running" ? <Loader2 className="size-4 animate-spin" /> : null}
            Test connection
          </Button>
          {test.kind === "ok" ? (
            <span className="text-success text-sm">{test.detail}</span>
          ) : test.kind === "err" ? (
            <span className="text-destructive font-mono text-xs break-all">
              {test.detail}
            </span>
          ) : null}
        </div>
      </section>

      <section className="border-destructive/30 space-y-3 rounded-lg border p-4">
        <h3 className="text-destructive text-sm font-semibold">Danger zone</h3>
        <p className="text-muted-foreground text-sm">
          Remove this connector from the workspace. (Session-scoped in this build —
          it returns on restart unless removed from the config file.)
        </p>
        {error ? (
          <p className="text-destructive font-mono text-xs break-all">{error}</p>
        ) : null}
        {confirming ? (
          <div className="flex items-center gap-2">
            <Button variant="destructive" onClick={handleRemove} disabled={removing}>
              {removing ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
              Confirm remove
            </Button>
            <Button variant="ghost" onClick={() => setConfirming(false)} disabled={removing}>
              Cancel
            </Button>
          </div>
        ) : (
          <Button variant="outline" onClick={() => setConfirming(true)}>
            <Trash2 className="size-4" /> Remove connector
          </Button>
        )}
      </section>
    </div>
  );
}

function Meta({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="bg-card px-3 py-2">
      <div className="text-muted-foreground text-[11px] tracking-wide uppercase">
        {label}
      </div>
      <div className="mt-0.5 font-medium">{children}</div>
    </div>
  );
}
