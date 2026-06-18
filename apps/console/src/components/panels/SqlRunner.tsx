import { useEffect, useRef, useState } from "react";
import { Clock, Loader2, Play, Rows3, Square } from "lucide-react";

import { useClients } from "@/lib/connection";
import { useNav } from "@/lib/nav";
import { streamQuery, type QueryResult } from "@/lib/query";
import { errMsg } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { PageHeader } from "@/components/PageHeader";
import { TableSurface } from "@/components/PanelShell";
import { ResultTable } from "@/components/ResultTable";

export function SqlRunner() {
  const { query } = useClients();
  const { pendingSql, consumePendingSql } = useNav();
  const [sql, setSql] = useState("SELECT 1 AS hello;");
  const [maxRows, setMaxRows] = useState(1000);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState(0);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const controller = useRef<AbortController | null>(null);

  // Seed the editor when another panel hands us SQL (e.g. "Open in SQL runner"
  // from an Activity record), then clear the queued value.
  useEffect(() => {
    if (pendingSql !== null) {
      setSql(pendingSql);
      setResult(null);
      setError(null);
      consumePendingSql();
    }
  }, [pendingSql, consumePendingSql]);

  async function run() {
    if (running || !sql.trim()) return;
    const ctrl = new AbortController();
    controller.current = ctrl;
    setRunning(true);
    setError(null);
    setResult(null);
    setProgress(0);
    try {
      const res = await streamQuery(query, sql, maxRows, {
        signal: ctrl.signal,
        onProgress: setProgress,
      });
      setResult(res);
    } catch (e) {
      if (!ctrl.signal.aborted) setError(errMsg(e));
    } finally {
      setRunning(false);
      controller.current = null;
    }
  }

  return (
    <div className="space-y-5">
      <PageHeader
        title="SQL runner"
        description="Run ad-hoc SQL against the workspace. Results stream live."
        actions={
          <>
            <div className="flex items-center gap-1.5">
              <span className="text-muted-foreground text-xs">max rows</span>
              <Input
                type="number"
                min={1}
                value={maxRows}
                onChange={(e) =>
                  setMaxRows(Math.max(1, Number(e.target.value) || 1))
                }
                className="h-8 w-24"
              />
            </div>
            {running ? (
              <Button variant="destructive" size="sm" onClick={() => controller.current?.abort()}>
                <Square className="size-4" />
                Cancel
              </Button>
            ) : (
              <Button size="sm" onClick={() => void run()} disabled={!sql.trim()}>
                <Play className="size-4" />
                Run
              </Button>
            )}
          </>
        }
      />

      <div className="bg-terminal overflow-hidden rounded-lg border shadow-sm">
        <div className="flex items-center justify-between border-b border-white/10 px-3 py-2">
          <span className="text-terminal-foreground/50 font-mono text-xs">
            query.sql
          </span>
          <span className="text-terminal-foreground/35 font-mono text-xs">
            ⌘/Ctrl + Enter to run
          </span>
        </div>
        <textarea
          value={sql}
          onChange={(e) => setSql(e.target.value)}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
              e.preventDefault();
              void run();
            }
          }}
          spellCheck={false}
          rows={6}
          placeholder="SELECT * FROM …"
          className="text-terminal-foreground placeholder:text-terminal-foreground/30 block min-h-[9rem] w-full resize-y bg-transparent px-4 py-3 font-mono text-sm leading-relaxed outline-none"
        />
      </div>

      <div className="text-muted-foreground flex items-center gap-4 text-sm">
        {running ? (
          <span className="flex items-center gap-1.5">
            <Loader2 className="size-4 animate-spin" />
            streaming… {progress} rows
          </span>
        ) : null}
        {result && !running ? (
          <>
            <span className="flex items-center gap-1.5 tabular-nums">
              <Rows3 className="size-4" />
              {result.rowsReturned} rows
            </span>
            {result.elapsedMs !== undefined ? (
              <span className="flex items-center gap-1.5 tabular-nums">
                <Clock className="size-4" />
                {result.elapsedMs.toFixed(1)} ms
              </span>
            ) : null}
            {result.truncated ? <Badge variant="warning">truncated</Badge> : null}
            {result.queryId ? (
              <span className="font-mono text-xs">{result.queryId}</span>
            ) : null}
          </>
        ) : null}
      </div>

      {error ? (
        <pre className="text-destructive border-destructive/30 bg-destructive/5 overflow-auto rounded-lg border p-3 text-sm whitespace-pre-wrap">
          {error}
        </pre>
      ) : null}

      {result && !error ? (
        result.columns.length > 0 ? (
          <TableSurface>
            <ResultTable columns={result.columns} rows={result.rows} />
          </TableSurface>
        ) : (
          <div className="text-muted-foreground bg-card rounded-lg border px-4 py-8 text-center text-sm">
            Query completed with no result columns.
          </div>
        )
      ) : null}
    </div>
  );
}
