import { useRef, useState, type ReactNode } from "react";
import { Clock, Loader2, Play, Rows3, Square } from "lucide-react";

import { cn } from "@/lib/utils";
import { useClients } from "@/lib/connection";
import { useAsync } from "@/lib/useAsync";
import {
  dimensionTypeLabel,
  errMsg,
  relationshipKindLabel,
  timeGrainLabel,
} from "@/lib/format";
import { streamSemanticQuery, type QueryResult } from "@/lib/query";
import type { ModelDescription } from "@/gen/pawrly/v1/semantic_pb";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ResultTable } from "@/components/ResultTable";
import { TableSurface } from "@/components/PanelShell";
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

function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <div className="text-muted-foreground text-[11px] font-medium tracking-wide uppercase">
      {children}
    </div>
  );
}

function Chip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "rounded-full border px-2.5 py-1 font-mono text-xs transition-colors",
        active
          ? "bg-primary text-primary-foreground border-primary"
          : "hover:bg-accent text-foreground",
      )}
    >
      {children}
    </button>
  );
}

function toggle(set: Set<string>, key: string): Set<string> {
  const next = new Set(set);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  return next;
}

/** Interactive builder: pick measures + dimensions (+ segments) and run a
 *  governed SemanticQuery, streaming the results. */
function QueryBuilder({ model }: { model: ModelDescription }) {
  const { semantic } = useClients();
  const [measures, setMeasures] = useState<Set<string>>(new Set());
  const [dims, setDims] = useState<Set<string>>(new Set());
  const [segs, setSegs] = useState<Set<string>>(new Set());
  const [limit, setLimit] = useState(100);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const controller = useRef<AbortController | null>(null);

  async function run() {
    if (running || measures.size === 0) return;
    const ctrl = new AbortController();
    controller.current = ctrl;
    setRunning(true);
    setError(null);
    setResult(null);
    try {
      const res = await streamSemanticQuery(
        semantic,
        {
          measures: [...measures].map((m) => `${model.name}.${m}`),
          dimensions: [...dims].map((d) => `${model.name}.${d}`),
          segments: [...segs].map((s) => `${model.name}.${s}`),
          limit,
        },
        limit,
        { signal: ctrl.signal },
      );
      setResult(res);
    } catch (e) {
      if (!ctrl.signal.aborted) setError(errMsg(e));
    } finally {
      setRunning(false);
      controller.current = null;
    }
  }

  return (
    <div className="bg-muted/30 space-y-3 rounded-lg border p-3">
      <SectionLabel>Run query</SectionLabel>

      <div className="space-y-1.5">
        <div className="text-muted-foreground text-xs">Measures</div>
        <div className="flex flex-wrap gap-1.5">
          {model.measures.map((m) => (
            <Chip
              key={m.name}
              active={measures.has(m.name)}
              onClick={() => setMeasures((s) => toggle(s, m.name))}
            >
              {m.name}
            </Chip>
          ))}
        </div>
      </div>

      <div className="space-y-1.5">
        <div className="text-muted-foreground text-xs">Group by (dimensions)</div>
        <div className="flex flex-wrap gap-1.5">
          {model.dimensions.map((d) => (
            <Chip
              key={d.name}
              active={dims.has(d.name)}
              onClick={() => setDims((s) => toggle(s, d.name))}
            >
              {d.name}
            </Chip>
          ))}
        </div>
      </div>

      {model.segments.length > 0 ? (
        <div className="space-y-1.5">
          <div className="text-muted-foreground text-xs">Segments</div>
          <div className="flex flex-wrap gap-1.5">
            {model.segments.map((s) => (
              <Chip
                key={s.name}
                active={segs.has(s.name)}
                onClick={() => setSegs((cur) => toggle(cur, s.name))}
              >
                {s.name}
              </Chip>
            ))}
          </div>
        </div>
      ) : null}

      <div className="flex items-center gap-2">
        <span className="text-muted-foreground text-xs">limit</span>
        <Input
          type="number"
          min={1}
          value={limit}
          onChange={(e) => setLimit(Math.max(1, Number(e.target.value) || 1))}
          className="h-8 w-24"
        />
        {running ? (
          <Button
            variant="destructive"
            size="sm"
            onClick={() => controller.current?.abort()}
          >
            <Square className="size-4" /> Cancel
          </Button>
        ) : (
          <Button size="sm" onClick={() => void run()} disabled={measures.size === 0}>
            <Play className="size-4" /> Run
          </Button>
        )}
        {measures.size === 0 ? (
          <span className="text-muted-foreground text-xs">
            Select at least one measure.
          </span>
        ) : null}
        {running ? (
          <span className="text-muted-foreground flex items-center gap-1.5 text-xs">
            <Loader2 className="size-3.5 animate-spin" /> running…
          </span>
        ) : null}
        {result && !running ? (
          <span className="text-muted-foreground flex items-center gap-3 text-xs">
            <span className="flex items-center gap-1 tabular-nums">
              <Rows3 className="size-3.5" /> {result.rowsReturned}
            </span>
            {result.elapsedMs !== undefined ? (
              <span className="flex items-center gap-1 tabular-nums">
                <Clock className="size-3.5" /> {result.elapsedMs.toFixed(1)} ms
              </span>
            ) : null}
          </span>
        ) : null}
      </div>

      {error ? (
        <pre className="text-destructive border-destructive/30 bg-destructive/5 overflow-auto rounded-md border p-2 text-xs whitespace-pre-wrap">
          {error}
        </pre>
      ) : null}
      {result && !error && result.columns.length > 0 ? (
        <TableSurface>
          <ResultTable columns={result.columns} rows={result.rows} maxHeight="40vh" />
        </TableSurface>
      ) : null}
    </div>
  );
}

function Body({ name }: { name: string }) {
  const { semantic } = useClients();
  const state = useAsync(() => semantic.describeModel({ name }), [name]);

  if (state.loading && !state.data) {
    return (
      <div className="text-muted-foreground flex items-center gap-2 py-10 text-sm">
        <Loader2 className="size-4 animate-spin" /> Loading model…
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
  const model = state.data?.model;
  if (!model) return null;

  return (
    <>
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2">
          {model.name}
          {model.source ? (
            <Badge variant="outline" className="font-mono">
              {model.source}
            </Badge>
          ) : null}
        </DialogTitle>
        <DialogDescription>
          {model.description || "No description."}
          {model.primaryKey.length > 0
            ? ` · primary key: ${model.primaryKey.join(", ")}`
            : ""}
        </DialogDescription>
      </DialogHeader>

      <div className="max-h-[70vh] space-y-5 overflow-y-auto pr-1">
        <QueryBuilder model={model} />

        <div className="space-y-1">
          <SectionLabel>Dimensions ({model.dimensions.length})</SectionLabel>
          <div className="overflow-hidden rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Type</TableHead>
                  <TableHead>Expr</TableHead>
                  <TableHead>Grains</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {model.dimensions.map((d) => (
                  <TableRow key={d.name}>
                    <TableCell className="font-mono font-medium">
                      {d.name}
                    </TableCell>
                    <TableCell>
                      <Badge variant="secondary">
                        {dimensionTypeLabel(d.type)}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-muted-foreground font-mono text-xs">
                      {d.expr}
                    </TableCell>
                    <TableCell className="text-muted-foreground text-xs">
                      {d.grains.length > 0
                        ? d.grains.map(timeGrainLabel).join(", ")
                        : "—"}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </div>

        <div className="space-y-1">
          <SectionLabel>Measures ({model.measures.length})</SectionLabel>
          <div className="overflow-hidden rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead>Agg</TableHead>
                  <TableHead>Expr</TableHead>
                  <TableHead>Format</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {model.measures.map((m) => (
                  <TableRow key={m.name}>
                    <TableCell className="font-mono font-medium">
                      {m.name}
                    </TableCell>
                    <TableCell>
                      <Badge variant="secondary">{m.agg}</Badge>
                    </TableCell>
                    <TableCell className="text-muted-foreground max-w-xs truncate font-mono text-xs">
                      {m.agg === "custom" ? m.customSql : m.expr}
                      {m.filters.length > 0
                        ? ` · filter: ${m.filters.join(", ")}`
                        : ""}
                    </TableCell>
                    <TableCell className="text-muted-foreground font-mono text-xs">
                      {m.format || "—"}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </div>

        {model.relationships.length > 0 ? (
          <div className="space-y-1">
            <SectionLabel>Relationships</SectionLabel>
            <div className="divide-border divide-y rounded-lg border text-sm">
              {model.relationships.map((r) => (
                <div
                  key={r.name}
                  className="flex flex-wrap items-center gap-2 px-3 py-2"
                >
                  <span className="font-mono font-medium">{r.name}</span>
                  <Badge variant="outline">{relationshipKindLabel(r.kind)}</Badge>
                  <span className="text-muted-foreground font-mono">
                    → {r.target}
                  </span>
                  <span className="text-muted-foreground font-mono text-xs">
                    on {r.on}
                  </span>
                </div>
              ))}
            </div>
          </div>
        ) : null}

        {model.segments.length > 0 ? (
          <div className="space-y-1">
            <SectionLabel>Segments</SectionLabel>
            <div className="divide-border divide-y rounded-lg border text-sm">
              {model.segments.map((s) => (
                <div key={s.name} className="px-3 py-2">
                  <span className="font-mono font-medium">{s.name}</span>
                  {s.description ? (
                    <span className="text-muted-foreground">
                      {" "}
                      — {s.description}
                    </span>
                  ) : null}
                </div>
              ))}
            </div>
          </div>
        ) : null}
      </div>
    </>
  );
}

export function SemanticModelDialog({
  name,
  onClose,
}: {
  name: string | null;
  onClose: () => void;
}) {
  return (
    <Dialog open={!!name} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-4xl">
        {name ? <Body name={name} /> : null}
      </DialogContent>
    </Dialog>
  );
}
