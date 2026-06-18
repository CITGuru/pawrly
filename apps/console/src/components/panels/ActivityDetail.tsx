import type { ReactNode } from "react";
import { ExternalLink, SquareTerminal } from "lucide-react";

import { buildTraceUrl, formatBytes } from "@/lib/format";
import { useConnection } from "@/lib/connection";
import { useNav } from "@/lib/nav";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

export type ActivityRecord = Record<string, string>;

// Columns surfaced explicitly; anything else falls through to "Other".
const STRIP = new Set([
  "operation",
  "status",
  "id",
  "at",
  "sql",
  "duration_ms",
  "rows_returned",
  "bytes",
  "interface",
]);
const META_ORDER = ["trace_id", "error_code", "principal", "param_keys"];

function dash(v: string | undefined): string {
  return v && v.length > 0 ? v : "—";
}

function fullTime(iso: string): string {
  if (!iso) return "—";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

function Metric({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="bg-muted/40 rounded-md px-3 py-2">
      <div className="text-muted-foreground text-[11px] tracking-wide uppercase">
        {label}
      </div>
      <div className="mt-0.5 font-medium tabular-nums">{value}</div>
    </div>
  );
}

function Field({
  label,
  value,
  href,
}: {
  label: string;
  value: string;
  href?: string | null;
}) {
  return (
    <div className="grid grid-cols-[8rem_1fr] gap-3 py-1.5 text-sm">
      <div className="text-muted-foreground">{label}</div>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          className="text-primary inline-flex items-center gap-1 font-mono break-all hover:underline"
        >
          {value}
          <ExternalLink className="size-3 shrink-0" />
        </a>
      ) : (
        <div className="font-mono break-all">{dash(value)}</div>
      )}
    </div>
  );
}

function prettyLabel(key: string): string {
  return key.replace(/_/g, " ").replace(/\bid\b/i, "ID");
}

export function ActivityDetail({
  record,
  onClose,
}: {
  record: ActivityRecord | null;
  onClose: () => void;
}) {
  const { traceUrlTemplate } = useConnection();
  const { openSql } = useNav();
  const ok = (record?.status || "").toLowerCase() === "ok";
  const bytes = record?.bytes ? formatBytes(Number(record.bytes)) : "—";
  const traceHref = record
    ? buildTraceUrl(traceUrlTemplate, record.trace_id ?? "")
    : null;

  // Any column we didn't surface elsewhere — keeps the modal complete if the
  // activity schema grows.
  const extras = record
    ? Object.keys(record).filter(
        (k) => !STRIP.has(k) && !META_ORDER.includes(k),
      )
    : [];

  return (
    <Dialog open={!!record} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        {record ? (
          <>
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <span className="capitalize">{record.operation || "operation"}</span>
                <Badge variant={ok ? "success" : "destructive"}>
                  {record.status || "—"}
                </Badge>
              </DialogTitle>
              <DialogDescription className="flex flex-wrap gap-x-3 font-mono">
                <span>{dash(record.id)}</span>
                <span>·</span>
                <span>{fullTime(record.at)}</span>
              </DialogDescription>
            </DialogHeader>

            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              <Metric
                label="Duration"
                value={record.duration_ms ? `${record.duration_ms} ms` : "—"}
              />
              <Metric label="Rows" value={dash(record.rows_returned)} />
              <Metric label="Bytes" value={bytes} />
              <Metric label="Interface" value={dash(record.interface)} />
            </div>

            <div>
              <div className="mb-1 flex items-center justify-between">
                <div className="text-muted-foreground text-[11px] tracking-wide uppercase">
                  SQL
                </div>
                {record.sql ? (
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-7"
                    onClick={() => openSql(record.sql)}
                  >
                    <SquareTerminal className="size-3.5" />
                    Open in SQL runner
                  </Button>
                ) : null}
              </div>
              <pre className="bg-terminal text-terminal-foreground max-h-60 overflow-auto rounded-md p-3 font-mono text-sm whitespace-pre-wrap">
                {record.sql || "—"}
              </pre>
            </div>

            <div className="divide-border divide-y border-t pt-1">
              {META_ORDER.map((k) => (
                <Field
                  key={k}
                  label={prettyLabel(k)}
                  value={record[k]}
                  href={k === "trace_id" ? traceHref : undefined}
                />
              ))}
              {extras.map((k) => (
                <Field key={k} label={prettyLabel(k)} value={record[k]} />
              ))}
            </div>
          </>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
