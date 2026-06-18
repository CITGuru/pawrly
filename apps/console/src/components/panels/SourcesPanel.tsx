import { useState } from "react";
import { ChevronRight } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import {
  formatCount,
  formatTimestamp,
  sourceKindLabel,
  sourceStatusOk,
} from "@/lib/format";
import type { SourceInfo } from "@/gen/pawrly/v1/common_pb";
import { PanelShell, TableSurface, EmptyHint } from "@/components/PanelShell";
import { StatRow, StatTile } from "@/components/StatTile";
import { MiniBar, StatusDot } from "@/components/dataviz";
import { SourceDetailDialog } from "./SourceDetailDialog";
import { TableDetailDialog, type TableRef } from "./TableDetailDialog";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export function SourcesPanel() {
  const { sources } = useClients();
  const [selectedSource, setSelectedSource] = useState<SourceInfo | null>(null);
  const [selectedTable, setSelectedTable] = useState<TableRef | null>(null);
  const state = useAsync(
    () => sources.listSources({}).then((r) => r.sources),
    [sources],
  );

  const list = state.data ?? [];
  const healthy = list.filter((s) => sourceStatusOk(s.status)).length;
  const totalTables = list.reduce((acc, s) => acc + Number(s.tableCount), 0);
  const maxTables = Math.max(1, ...list.map((s) => Number(s.tableCount)));

  return (
    <PanelShell
      title="Sources"
      description="Registered sources and their health."
      loading={state.loading}
      error={state.error}
      onReload={state.reload}
      stats={
        state.data ? (
          <StatRow>
            <StatTile label="Sources" value={list.length} />
            <StatTile label="Healthy" value={healthy} tone="success" />
            <StatTile
              label="Unavailable"
              value={list.length - healthy}
              tone={list.length - healthy > 0 ? "destructive" : "default"}
            />
            <StatTile label="Tables" value={formatCount(totalTables)} />
          </StatRow>
        ) : null
      }
    >
      {state.data && list.length === 0 ? (
        <EmptyHint>No sources registered in this workspace.</EmptyHint>
      ) : null}
      {list.length > 0 ? (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Kind</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="text-right">Tables</TableHead>
                <TableHead>Registered</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {list.map((s) => (
                <TableRow
                  key={s.name}
                  onClick={() => setSelectedSource(s)}
                  className="group cursor-pointer"
                >
                  <TableCell className="font-medium">{s.name}</TableCell>
                  <TableCell>
                    <div className="flex items-center gap-1.5">
                      <Badge variant="outline">{sourceKindLabel(s.kind)}</Badge>
                      {s.subKind ? (
                        <Badge variant="secondary">{s.subKind}</Badge>
                      ) : null}
                    </div>
                  </TableCell>
                  <TableCell>
                    <StatusDot
                      ok={sourceStatusOk(s.status)}
                      label={sourceStatusOk(s.status) ? "ok" : "unavailable"}
                      title={s.statusDetail}
                    />
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center justify-end gap-2">
                      <span className="tabular-nums">
                        {formatCount(s.tableCount)}
                      </span>
                      <MiniBar value={Number(s.tableCount)} max={maxTables} />
                    </div>
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {formatTimestamp(s.registeredAt)}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    <ChevronRight className="size-4 opacity-0 transition-opacity group-hover:opacity-100" />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableSurface>
      ) : null}

      <SourceDetailDialog
        source={selectedSource}
        onClose={() => setSelectedSource(null)}
        onOpenTable={(name) => {
          setSelectedSource(null);
          setSelectedTable(name);
        }}
      />
      <TableDetailDialog
        name={selectedTable}
        onClose={() => setSelectedTable(null)}
      />
    </PanelShell>
  );
}
