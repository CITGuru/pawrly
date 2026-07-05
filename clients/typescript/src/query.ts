import type { QueryResult } from "./result.js";

export type Row = Record<string, unknown>;

/** Mutable completion metadata, filled by a transport's row generator as it
 * drains — read once iteration finishes. */
export interface QueryMeta {
  columns: string[];
  rowCount: number;
  truncated: boolean;
}

/**
 * A streaming query result. Async-iterate the rows (memory-bounded over gRPC),
 * or {@link collect} them into a {@link QueryResult}. `id` is the server-assigned
 * query id for {@link PawrlyClient.cancel} — populated over gRPC, empty over REST
 * (which has no query id).
 */
export class QueryHandle implements AsyncIterable<Row> {
  constructor(
    readonly id: string,
    private readonly stream: AsyncIterable<Row>,
    private readonly meta: QueryMeta,
  ) {}

  [Symbol.asyncIterator](): AsyncIterator<Row> {
    return this.stream[Symbol.asyncIterator]();
  }

  async collect(): Promise<QueryResult> {
    const rows: Row[] = [];
    for await (const row of this.stream) {
      rows.push(row);
    }
    return {
      columns: this.meta.columns,
      rows,
      rowCount: this.meta.rowCount,
      truncated: this.meta.truncated,
    };
  }
}
