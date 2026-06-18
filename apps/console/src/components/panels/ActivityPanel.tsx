import { useMemo, useState } from "react";
import { ChevronRight } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { streamQuery } from "@/lib/query";
import { PanelShell, TableSurface, EmptyHint } from "@/components/PanelShell";
import { StatusDot } from "@/components/dataviz";
import { ActivityDetail, type ActivityRecord } from "./ActivityDetail";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const ACTIVITY_SQL =
  "SELECT * FROM system.activity ORDER BY at DESC LIMIT 200";

function shortTime(iso: string): string {
  if (!iso) return "—";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleTimeString();
}

export function ActivityPanel() {
  const { query } = useClients();
  const state = useAsync(
    () => streamQuery(query, ACTIVITY_SQL, 200, {}),
    [query],
  );
  const [selected, setSelected] = useState<ActivityRecord | null>(null);

  const records = useMemo<ActivityRecord[]>(() => {
    if (!state.data) return [];
    const { columns, rows } = state.data;
    return rows.map((row) =>
      Object.fromEntries(columns.map((c, i) => [c, row[i] ?? ""])),
    );
  }, [state.data]);

  // `system.activity` only exists when the table sink is enabled.
  if (state.error) {
    return (
      <PanelShell
        title="Activity"
        description="Recent query activity from system.activity."
        loading={state.loading}
        onReload={state.reload}
      >
        <div className="bg-card space-y-3 rounded-lg border p-6">
          <p className="text-sm font-medium">Activity table is not enabled.</p>
          <p className="text-muted-foreground text-sm">
            Turn on the table sink in your workspace config:
          </p>
          <pre className="bg-terminal text-terminal-foreground overflow-auto rounded-md p-3 text-sm">
            {`observability:\n  activity:\n    enabled: true\n    sinks: [tracing, table]`}
          </pre>
          <p className="text-muted-foreground font-mono text-xs break-all">
            {state.error}
          </p>
        </div>
      </PanelShell>
    );
  }

  return (
    <PanelShell
      title="Activity"
      description="Recent query activity. Select a row for the full record."
      loading={state.loading}
      onReload={state.reload}
    >
      {state.data && records.length === 0 ? (
        <EmptyHint>No recorded activity yet.</EmptyHint>
      ) : null}
      {records.length > 0 ? (
        <TableSurface>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Operation</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>SQL</TableHead>
                <TableHead className="text-right">Rows</TableHead>
                <TableHead className="text-right">Duration</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {records.map((r, i) => {
                const ok = (r.status || "").toLowerCase() === "ok";
                return (
                  <TableRow
                    key={r.id || i}
                    onClick={() => setSelected(r)}
                    className="group cursor-pointer"
                  >
                    <TableCell className="text-muted-foreground tabular-nums">
                      {shortTime(r.at)}
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
                    <TableCell className="text-muted-foreground">
                      <ChevronRight className="size-4 opacity-0 transition-opacity group-hover:opacity-100" />
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </TableSurface>
      ) : null}

      <ActivityDetail
        record={selected}
        onClose={() => setSelected(null)}
      />
    </PanelShell>
  );
}
