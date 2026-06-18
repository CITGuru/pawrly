import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import {
  cacheModeLabel,
  formatBytes,
  formatCount,
  formatTimestamp,
  tableNameString,
} from "@/lib/format";
import { PanelShell, TableSurface, EmptyHint } from "@/components/PanelShell";
import { StatRow, StatTile } from "@/components/StatTile";
import { MiniBar } from "@/components/dataviz";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const MATERIALIZED_SCHEMA = "materialized";

interface CacheEntriesPanelProps {
  /** Show materialized entries (schema == "materialized") vs cache entries. */
  materialized: boolean;
}

export function CacheEntriesPanel({ materialized }: CacheEntriesPanelProps) {
  const { cache } = useClients();
  const state = useAsync(
    () =>
      cache
        .listEntries({})
        .then((r) =>
          r.entries.filter(
            (e) => (e.name?.schema === MATERIALIZED_SCHEMA) === materialized,
          ),
        ),
    [cache, materialized],
  );

  const list = state.data ?? [];
  const totalRows = list.reduce((acc, e) => acc + Number(e.rowCount), 0);
  const totalBytes = list.reduce((acc, e) => acc + Number(e.sizeBytes), 0);
  const maxBytes = Math.max(1, ...list.map((e) => Number(e.sizeBytes)));

  const title = materialized ? "Materialized" : "Cache";
  const description = materialized
    ? "Self-backed materialized tables (materialized.*)."
    : "Cached table entries in the workspace manifest.";

  return (
    <PanelShell
      title={title}
      description={description}
      loading={state.loading}
      error={state.error}
      onReload={state.reload}
      stats={
        state.data ? (
          <StatRow>
            <StatTile
              label={materialized ? "Tables" : "Entries"}
              value={list.length}
            />
            <StatTile label="Rows" value={formatCount(totalRows)} />
            <StatTile label="Size" value={formatBytes(totalBytes)} />
          </StatRow>
        ) : null
      }
    >
      {state.data && list.length === 0 ? (
        <EmptyHint>
          {materialized
            ? "No materialized tables. Create one with `pawrly materialize`."
            : "No cache entries in this workspace."}
        </EmptyHint>
      ) : null}
      {list.length > 0 ? (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                {!materialized ? <TableHead>Mode</TableHead> : null}
                <TableHead className="text-right">Rows</TableHead>
                <TableHead className="text-right">Size</TableHead>
                <TableHead className="text-right">Files</TableHead>
                <TableHead>Written</TableHead>
                {!materialized ? <TableHead>Expires</TableHead> : null}
              </TableRow>
            </TableHeader>
            <TableBody>
              {list.map((e) => (
                <TableRow key={tableNameString(e.name)}>
                  <TableCell className="font-mono font-medium">
                    {tableNameString(e.name)}
                  </TableCell>
                  {!materialized ? (
                    <TableCell>
                      <Badge variant="secondary">
                        {cacheModeLabel(e.mode)}
                      </Badge>
                    </TableCell>
                  ) : null}
                  <TableCell className="text-right tabular-nums">
                    {formatCount(e.rowCount)}
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center justify-end gap-2">
                      <span className="tabular-nums">
                        {formatBytes(e.sizeBytes)}
                      </span>
                      <MiniBar value={Number(e.sizeBytes)} max={maxBytes} />
                    </div>
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {e.fileCount}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {formatTimestamp(e.writtenAt)}
                  </TableCell>
                  {!materialized ? (
                    <TableCell className="text-muted-foreground">
                      {formatTimestamp(e.expiresAt)}
                    </TableCell>
                  ) : null}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableSurface>
      ) : null}
    </PanelShell>
  );
}
