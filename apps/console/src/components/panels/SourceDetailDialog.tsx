import type { ReactNode } from "react";
import { ChevronRight, Loader2 } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import {
  formatCount,
  formatTimestamp,
  sourceKindLabel,
  sourceStatusOk,
  tableNameString,
} from "@/lib/format";
import type { SourceInfo } from "@/gen/pawrly/v1/common_pb";
import { Badge } from "@/components/ui/badge";
import { StatusDot } from "@/components/dataviz";
import type { TableRef } from "./TableDetailDialog";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

function Meta({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="bg-muted/40 rounded-md px-3 py-2">
      <div className="text-muted-foreground text-[11px] tracking-wide uppercase">
        {label}
      </div>
      <div className="mt-0.5 font-medium">{children}</div>
    </div>
  );
}

function Tables({
  source,
  onOpenTable,
}: {
  source: string;
  onOpenTable: (name: TableRef) => void;
}) {
  const { catalog } = useClients();
  const state = useAsync(
    () => catalog.listTables({ source }).then((r) => r.tables),
    [source],
  );

  if (state.loading && !state.data) {
    return (
      <div className="text-muted-foreground flex items-center gap-2 py-4 text-sm">
        <Loader2 className="size-4 animate-spin" /> Loading tables…
      </div>
    );
  }
  if (state.error) {
    return (
      <p className="text-destructive font-mono text-sm break-all">
        {state.error}
      </p>
    );
  }
  const tables = state.data ?? [];
  if (tables.length === 0) {
    return <p className="text-muted-foreground text-sm">No tables.</p>;
  }
  return (
    <div className="divide-border divide-y rounded-lg border">
      {tables.map((t) => (
        <button
          key={tableNameString(t.name)}
          onClick={() => t.name && onOpenTable(t.name)}
          className="hover:bg-muted/50 group flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm"
        >
          <span className="font-mono">{tableNameString(t.name)}</span>
          <span className="text-muted-foreground flex items-center gap-2">
            {t.description ? (
              <span className="hidden max-w-xs truncate sm:inline">
                {t.description}
              </span>
            ) : null}
            <ChevronRight className="size-4 opacity-0 transition-opacity group-hover:opacity-100" />
          </span>
        </button>
      ))}
    </div>
  );
}

export function SourceDetailDialog({
  source,
  onClose,
  onOpenTable,
}: {
  source: SourceInfo | null;
  onClose: () => void;
  onOpenTable: (name: TableRef) => void;
}) {
  return (
    <Dialog open={!!source} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        {source ? (
          <>
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                {source.name}
                <Badge variant="outline">{sourceKindLabel(source.kind)}</Badge>
                {source.subKind ? (
                  <Badge variant="secondary">{source.subKind}</Badge>
                ) : null}
              </DialogTitle>
              <DialogDescription>
                {source.statusDetail || "Source configuration and tables."}
              </DialogDescription>
            </DialogHeader>

            <div className="max-h-[65vh] space-y-4 overflow-y-auto pr-1">
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                <Meta label="Kind">
                  {sourceKindLabel(source.kind)}
                  {source.subKind ? (
                    <span className="text-muted-foreground"> · {source.subKind}</span>
                  ) : null}
                </Meta>
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

              <div className="text-muted-foreground text-[11px] font-medium tracking-wide uppercase">
                Tables
              </div>
              <Tables source={source.name} onOpenTable={onOpenTable} />
            </div>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
