import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { EmptyHint } from "@/components/PanelShell";

interface ResultTableProps {
  columns: string[];
  rows: string[][];
  maxHeight?: string;
}

/**
 * Render tabular results. Every cell is plain text rendered by React (escaped
 * by default) — never `dangerouslySetInnerHTML`, since cell values are
 * arbitrary data from any source the engine can read.
 */
export function ResultTable({
  columns,
  rows,
  maxHeight = "60vh",
}: ResultTableProps) {
  if (columns.length === 0) {
    return <EmptyHint>No columns returned.</EmptyHint>;
  }
  return (
    <div className="overflow-auto" style={{ maxHeight }}>
      <Table>
        <TableHeader className="bg-muted sticky top-0 z-10">
          <TableRow>
            <TableHead className="text-muted-foreground w-12 text-right">
              #
            </TableHead>
            {columns.map((c) => (
              <TableHead key={c} className="font-mono">
                {c}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((row, i) => (
            <TableRow key={i}>
              <TableCell className="text-muted-foreground text-right tabular-nums">
                {i + 1}
              </TableCell>
              {row.map((cell, j) => (
                <TableCell key={j} className="font-mono">
                  {cell}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
