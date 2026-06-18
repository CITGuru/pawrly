import { Loader2 } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import { formatCount, sourceKindLabel, tableNameString } from "@/lib/format";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export interface TableRef {
  schema: string;
  table: string;
}

function Body({ name }: { name: TableRef }) {
  const { catalog } = useClients();
  const state = useAsync(
    () => catalog.describeTable({ name }),
    [name.schema, name.table],
  );

  if (state.loading && !state.data) {
    return (
      <div className="text-muted-foreground flex items-center gap-2 py-10 text-sm">
        <Loader2 className="size-4 animate-spin" /> Loading table…
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
  if (!state.data) return null;

  const { table, columns, examples, wiki } = state.data;
  return (
    <>
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2 font-mono">
          {tableNameString(table?.name)}
          {table ? (
            <Badge variant="outline">{sourceKindLabel(table.kind)}</Badge>
          ) : null}
          {table?.cached ? <Badge variant="success">cached</Badge> : null}
        </DialogTitle>
        <DialogDescription>
          {table?.description || "No description."}
          {table?.rowCountEstimate !== undefined
            ? ` · ~${formatCount(table.rowCountEstimate)} rows`
            : ""}
        </DialogDescription>
      </DialogHeader>

      <div className="text-muted-foreground text-[11px] font-medium tracking-wide uppercase">
        Columns ({columns.length})
      </div>
      <div className="max-h-[50vh] overflow-auto rounded-lg border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Column</TableHead>
              <TableHead>Type</TableHead>
              <TableHead>Null</TableHead>
              <TableHead>Filter</TableHead>
              <TableHead>Description</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {columns.map((c) => (
              <TableRow key={c.name}>
                <TableCell className="font-mono font-medium">
                  {c.name}
                </TableCell>
                <TableCell className="text-muted-foreground font-mono">
                  {c.dataType}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {c.nullable ? "yes" : "no"}
                </TableCell>
                <TableCell className="space-x-1">
                  {c.isFilterPushable ? (
                    <Badge variant="secondary">pushdown</Badge>
                  ) : null}
                  {c.isRequiredFilter ? (
                    <Badge variant="warning">required</Badge>
                  ) : null}
                  {!c.isFilterPushable && !c.isRequiredFilter ? (
                    <span className="text-muted-foreground">—</span>
                  ) : null}
                </TableCell>
                <TableCell className="text-muted-foreground max-w-xs truncate">
                  {c.description || "—"}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      {examples.length > 0 ? (
        <div className="space-y-1">
          <div className="text-muted-foreground text-[11px] font-medium tracking-wide uppercase">
            Examples
          </div>
          <pre className="bg-terminal text-terminal-foreground max-h-40 overflow-auto rounded-md p-3 font-mono text-xs whitespace-pre-wrap">
            {examples.join("\n")}
          </pre>
        </div>
      ) : null}

      {wiki ? (
        <div className="space-y-1">
          <div className="text-muted-foreground text-[11px] font-medium tracking-wide uppercase">
            Notes
          </div>
          <p className="text-sm whitespace-pre-wrap">{wiki}</p>
        </div>
      ) : null}
    </>
  );
}

export function TableDetailDialog({
  name,
  onClose,
}: {
  name: TableRef | null;
  onClose: () => void;
}) {
  return (
    <Dialog open={!!name} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-3xl">
        {name ? <Body name={name} /> : null}
      </DialogContent>
    </Dialog>
  );
}
