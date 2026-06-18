import { useState } from "react";
import { ChevronRight } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { formatCount, sourceKindLabel, tableNameString } from "@/lib/format";
import { PanelShell, TableSurface, EmptyHint } from "@/components/PanelShell";
import { StatRow, StatTile } from "@/components/StatTile";
import { MiniBar } from "@/components/dataviz";
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

export function CatalogPanel() {
  const { catalog } = useClients();
  const [selected, setSelected] = useState<TableRef | null>(null);
  const state = useAsync(
    () => catalog.listTables({}).then((r) => r.tables),
    [catalog],
  );

  const list = state.data ?? [];
  const cached = list.filter((t) => t.cached).length;
  const schemas = new Set(list.map((t) => t.name?.schema ?? "")).size;
  const maxRows = Math.max(
    1,
    ...list.map((t) => (t.rowCountEstimate ? Number(t.rowCountEstimate) : 0)),
  );

  return (
    <PanelShell
      title="Catalog"
      description="SQL tables across all sources."
      loading={state.loading}
      error={state.error}
      onReload={state.reload}
      stats={
        state.data ? (
          <StatRow>
            <StatTile label="Tables" value={list.length} />
            <StatTile label="Cached" value={cached} />
            <StatTile label="Schemas" value={schemas} />
          </StatRow>
        ) : null
      }
    >
      {state.data && list.length === 0 ? (
        <EmptyHint>No tables in the catalog.</EmptyHint>
      ) : null}
      {list.length > 0 ? (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Table</TableHead>
                <TableHead>Kind</TableHead>
                <TableHead>Description</TableHead>
                <TableHead className="text-right">~ Rows</TableHead>
                <TableHead>Cached</TableHead>
                <TableHead>Required filters</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {list.map((t) => (
                <TableRow
                  key={tableNameString(t.name)}
                  onClick={() => t.name && setSelected(t.name)}
                  className="group cursor-pointer"
                >
                  <TableCell className="font-mono font-medium">
                    {tableNameString(t.name)}
                  </TableCell>
                  <TableCell>
                    <Badge variant="outline">{sourceKindLabel(t.kind)}</Badge>
                  </TableCell>
                  <TableCell className="text-muted-foreground max-w-sm truncate">
                    {t.description || "—"}
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center justify-end gap-2">
                      <span className="tabular-nums">
                        {t.rowCountEstimate !== undefined
                          ? formatCount(t.rowCountEstimate)
                          : "—"}
                      </span>
                      {t.rowCountEstimate !== undefined ? (
                        <MiniBar
                          value={Number(t.rowCountEstimate)}
                          max={maxRows}
                          tone="muted"
                        />
                      ) : null}
                    </div>
                  </TableCell>
                  <TableCell>
                    {t.cached ? (
                      <Badge variant="success">cached</Badge>
                    ) : (
                      <span className="text-muted-foreground">—</span>
                    )}
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {t.requiredFilters.length > 0
                      ? t.requiredFilters.join(", ")
                      : "—"}
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

      <TableDetailDialog name={selected} onClose={() => setSelected(null)} />
    </PanelShell>
  );
}
