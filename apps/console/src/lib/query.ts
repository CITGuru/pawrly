import type { Duration } from "@bufbuild/protobuf/wkt";

import type { Clients } from "./clients";
import type { Error as ProtoError } from "@/gen/pawrly/v1/common_pb";
import type { QueryResponse } from "@/gen/pawrly/v1/query_pb";
import { decodeBatch } from "./arrow";

export interface QueryResult {
  columns: string[];
  rows: string[][];
  rowsReturned: number;
  truncated: boolean;
  elapsedMs?: number;
  queryId?: string;
}

/** Consume a server stream of QueryResponse, decoding Arrow IPC frames. Shared
 *  by the raw SQL runner and the semantic-query runner (both stream the same
 *  QueryResponse oneof). */
async function consumeStream(
  stream: AsyncIterable<QueryResponse>,
  maxRows: number,
  onProgress?: (rows: number) => void,
): Promise<QueryResult> {
  let columns: string[] = [];
  const rows: string[][] = [];
  let truncated = false;
  let rowsReturned = 0;
  let elapsedMs: number | undefined;
  let queryId: string | undefined;

  for await (const res of stream) {
    switch (res.payload.case) {
      case "started":
        queryId = res.payload.value.queryId;
        break;
      case "ipcStream": {
        const batch = decodeBatch(res.payload.value);
        if (columns.length === 0) columns = batch.columns;
        for (const r of batch.rows) {
          if (rows.length < maxRows) rows.push(r);
        }
        onProgress?.(rows.length);
        break;
      }
      case "completed":
        rowsReturned = Number(res.payload.value.rowsReturned);
        truncated = res.payload.value.truncated;
        if (res.payload.value.elapsed) {
          elapsedMs = durationMs(res.payload.value.elapsed);
        }
        break;
      case "error":
        throw new Error(protoErrorMessage(res.payload.value));
    }
  }

  return {
    columns,
    rows,
    rowsReturned: rowsReturned || rows.length,
    truncated,
    elapsedMs,
    queryId,
  };
}

function durationMs(d: Duration): number {
  return Number(d.seconds) * 1000 + d.nanos / 1e6;
}

function protoErrorMessage(e: ProtoError): string {
  const base = e.code ? `${e.code}: ${e.message}` : e.message;
  return e.hint ? `${base} (${e.hint})` : base;
}

/**
 * Stream `QueryService.Query`, decoding each Arrow IPC frame into rows. Caps the
 * rendered rows at `maxRows` (also passed to the server). Aborting via
 * `opts.signal` cancels the underlying gRPC-Web call.
 */
export async function streamQuery(
  query: Clients["query"],
  sql: string,
  maxRows: number,
  opts: { signal?: AbortSignal; onProgress?: (rows: number) => void } = {},
): Promise<QueryResult> {
  return consumeStream(
    query.query({ sql, maxRows: BigInt(maxRows) }, { signal: opts.signal }),
    maxRows,
    opts.onProgress,
  );
}

export interface SemanticQueryInput {
  measures: string[];
  dimensions: string[];
  segments?: string[];
  limit?: number;
}

/** Stream `SemanticService.SemanticQuery` — the governed analog of streamQuery.
 *  Members are fully-qualified (`model.measure` / `model.dimension`). */
export async function streamSemanticQuery(
  semantic: Clients["semantic"],
  input: SemanticQueryInput,
  maxRows: number,
  opts: { signal?: AbortSignal; onProgress?: (rows: number) => void } = {},
): Promise<QueryResult> {
  return consumeStream(
    semantic.semanticQuery(
      {
        measures: input.measures,
        dimensions: input.dimensions,
        segments: input.segments ?? [],
        limit: input.limit !== undefined ? BigInt(input.limit) : undefined,
      },
      { signal: opts.signal },
    ),
    maxRows,
    opts.onProgress,
  );
}
