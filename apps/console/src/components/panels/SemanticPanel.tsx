import { useState } from "react";
import { ChevronRight } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { PanelShell, TableSurface, EmptyHint } from "@/components/PanelShell";
import { StatRow, StatTile } from "@/components/StatTile";
import { SemanticModelDialog } from "./SemanticModelDialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export function SemanticPanel() {
  const { semantic } = useClients();
  const [selected, setSelected] = useState<string | null>(null);
  const state = useAsync(
    () => semantic.listModels({}).then((r) => r.models),
    [semantic],
  );

  const list = state.data ?? [];
  const totalDimensions = list.reduce((acc, m) => acc + m.dimensionCount, 0);
  const totalMeasures = list.reduce((acc, m) => acc + m.measureCount, 0);

  return (
    <PanelShell
      title="Semantic"
      description="Governed business models — dimensions and measures over the catalog."
      loading={state.loading}
      error={state.error}
      onReload={state.reload}
      stats={
        state.data ? (
          <StatRow>
            <StatTile label="Models" value={list.length} />
            <StatTile label="Dimensions" value={totalDimensions} />
            <StatTile label="Measures" value={totalMeasures} />
          </StatRow>
        ) : null
      }
    >
      {state.data && list.length === 0 ? (
        <EmptyHint>
          No semantic models defined. Add a <code>semantic:</code> block to your
          workspace config to expose governed models.
        </EmptyHint>
      ) : null}
      {list.length > 0 ? (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Model</TableHead>
                <TableHead>Source</TableHead>
                <TableHead className="text-right">Dimensions</TableHead>
                <TableHead className="text-right">Measures</TableHead>
                <TableHead>Description</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {list.map((m) => (
                <TableRow
                  key={m.name}
                  onClick={() => setSelected(m.name)}
                  className="group cursor-pointer"
                >
                  <TableCell className="font-mono font-medium">
                    {m.name}
                  </TableCell>
                  <TableCell className="text-muted-foreground font-mono">
                    {m.source || "—"}
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {m.dimensionCount}
                  </TableCell>
                  <TableCell className="text-right tabular-nums">
                    {m.measureCount}
                  </TableCell>
                  <TableCell className="text-muted-foreground max-w-sm truncate">
                    {m.description || "—"}
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

      <SemanticModelDialog name={selected} onClose={() => setSelected(null)} />
    </PanelShell>
  );
}
